//! Pixel Map (PRD §6 "Pixel Out", FR-S7): samples regions of an image for
//! the fixtures of a DMX patch and outputs their DMX slot values as
//! channels, optionally sending every patched universe over Art-Net or
//! sACN itself.
//!
//! The patch (`docs/dmx.md`) is a TOML file read and laid out on a watcher
//! thread with its fixture types (`fixtures/` in the project, then the
//! built-ins), so no file is read on the render thread (rule 1). One
//! compute pass averages each pixel's cell of the image into a storage
//! buffer, copied into one of three readback buffers that are mapped after
//! the frame is submitted and read on a later cook, as Analyze does: the
//! values trail the image by one frame or two live, exactly one offline.
//! Universes are sent from `atd_io::dmx` threads, never from the cook.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{ImgToSigCtx, ImgToSigOp, Params};
use atd_io::dmx::{DmxSender, Protocol};
use atd_io::fixture::Mapping;

use crate::watch::{Watcher, watch};

const PROTOCOLS: &[&str] = &["off", "artnet", "sacn"];

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.pixel_map".into(),
        family: OpFamily::Img,
        label: "Pixel Map".into(),
        summary: "Samples image regions for the fixtures and LED pixels of a DMX patch file and outputs their DMX slot values (0–1) as channels `u<universe>_<slot>`; optionally sends every patched universe over Art-Net or sACN. Values trail the image by a frame or two (GPU readback).".into(),
        version: 1,
        params: vec![
            ParamSchema::path("patch", "Patch")
                .help("The patch file (TOML, `docs/dmx.md`): fixtures with type, universe, address and the point, line or rectangle they sample. Fixture types come from the project's `fixtures/` folder, then the built-ins."),
            ParamSchema::int("universe", "Channels Of", 0, Some(0), Some(63_999))
                .help("The universe whose slots become output channels (to wire into DMX Out, which sends one universe); 0 outputs every patched universe in order."),
            ParamSchema::menu("protocol", "Send", PROTOCOLS, "off")
                .help("`off`: channels only (wire them to DMX Out); `artnet` or `sacn`: also send every patched universe (at most 44 times a second, repeated every second)."),
            ParamSchema::string("host", "Host", "")
                .help("Destination node; empty broadcasts (Art-Net) or uses each universe's multicast group (sACN)."),
            ParamSchema::int("port", "Port", 0, Some(0), Some(65_535))
                .help("UDP port; 0 for the protocol's standard port."),
        ],
        inputs: vec![SocketSchema {
            name: "in".into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        }],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Sig,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 80,
        time_dependent: false,
        sink: true,
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> cells: array<vec4f>;
@group(0) @binding(2) var<storage, read_write> colours: array<vec4f>;

// Each pixel averages a 4 x 4 grid of texels over its cell (centre xy,
// half size zw, in 0-1 image coordinates); a point cell reads one texel.
@compute @workgroup_size(64)
fn sample(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= arrayLength(&cells)) {
        return;
    }
    let c = cells[i];
    let size = textureDimensions(src);
    let dims = vec2f(size);
    let last = vec2i(size) - vec2i(1);
    var sum = vec4f(0.0);
    for (var y = 0u; y < 4u; y++) {
        for (var x = 0u; x < 4u; x++) {
            let o = (vec2f(f32(x), f32(y)) + 0.5) * 0.5 - 1.0;
            let uv = c.xy + o * c.zw;
            let p = clamp(vec2i(floor(uv * dims)), vec2i(0), last);
            sum += textureLoad(src, p, 0);
        }
    }
    colours[i] = sum / 16.0;
}
"#;

const IDLE: u8 = 0;
const RECORDED: u8 = 1;
const MAPPING: u8 = 2;
const READY: u8 = 3;

/// A readback taking longer than this is reported as an error.
const STUCK: std::time::Duration = std::time::Duration::from_secs(5);

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    frame: u64,
    since: std::time::Instant,
}

/// GPU resources and output state for one laid-out patch.
struct Patched {
    mapping: Mapping,
    cells: wgpu::Buffer,
    colours_gpu: wgpu::Buffer,
    slots: Vec<Slot>,
    colours: Vec<[f32; 4]>,
    universes: Vec<Vec<u8>>,
    /// Channel names, `u<universe>_<slot>`, per universe.
    names: Vec<Vec<String>>,
    /// The frame of the values in `universes`.
    measured: Option<u64>,
}

struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

/// DMX senders, one per universe, and the settings they were opened with
/// (shared with PNT Pixel Out).
#[derive(Default)]
pub struct Senders {
    config: Option<(String, String, i64, Vec<u16>)>,
    senders: Vec<DmxSender>,
}

impl std::fmt::Debug for Senders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Senders")
            .field("config", &self.config)
            .field("senders", &self.senders.len())
            .finish()
    }
}

enum Build {
    Building(std::sync::mpsc::Receiver<Result<Pipeline, String>>),
    Ready(Pipeline),
    Failed(String),
}

pub struct PixelMap {
    build: Build,
    watcher: Option<Watcher<Mapping>>,
    patch: Option<Patched>,
    patch_error: Option<String>,
    senders: Senders,
    last_input: Option<wgpu::Texture>,
    stale: bool,
}

impl std::fmt::Debug for PixelMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelMap")
            .field("patch_error", &self.patch_error)
            .finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device) -> Box<dyn ImgToSigOp> {
    let (tx, rx) = std::sync::mpsc::channel();
    let device = device.clone();
    // Shader compilers such as FXC can take seconds (rule 1).
    let spawned = std::thread::Builder::new()
        .name("atd-pixel-map-build".into())
        .spawn(move || {
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let p = build_pipeline(&device);
                match pollster::block_on(scope.pop()) {
                    Some(e) => Err(format!("Pixel Map pipeline: {e}")),
                    None => Ok(p),
                }
            }))
            .unwrap_or_else(|_| Err("Pixel Map pipeline build panicked".into()));
            let _ = tx.send(built);
        });
    Box::new(PixelMap {
        build: match spawned {
            Ok(_) => Build::Building(rx),
            Err(e) => Build::Failed(format!("cannot start build thread: {e}")),
        },
        watcher: None,
        patch: None,
        patch_error: None,
        senders: Senders::default(),
        last_input: None,
        stale: false,
    })
}

fn build_pipeline(device: &wgpu::Device) -> Pipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("img.pixel_map"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("img.pixel_map"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            storage(1, true),
            storage(2, false),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("img.pixel_map"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("img.pixel_map"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("sample"),
        compilation_options: Default::default(),
        cache: None,
    });
    Pipeline { pipeline, layout }
}

/// Builds the GPU side of a laid-out patch (only when the patch changes).
fn upload(device: &wgpu::Device, queue: &wgpu::Queue, mapping: Mapping) -> Patched {
    let n = mapping.cells.len().max(1) as u64;
    let buffer = |size: u64, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.pixel_map"),
            size,
            usage,
            mapped_at_creation: false,
        })
    };
    let cells = buffer(
        n * 16,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );
    let bytes: Vec<u8> = mapping
        .cells
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    queue.write_buffer(&cells, 0, &bytes);
    let colours_gpu = buffer(
        n * 16,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let slots = (0..3)
        .map(|_| Slot {
            buffer: buffer(
                n * 16,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            state: Arc::new(AtomicU8::new(IDLE)),
            frame: 0,
            since: std::time::Instant::now(),
        })
        .collect();
    let universes = mapping
        .universes
        .iter()
        .map(|&(_, len)| vec![0u8; len])
        .collect();
    let names = mapping
        .universes
        .iter()
        .map(|&(u, len)| (1..=len).map(|s| format!("u{u}_{s}")).collect())
        .collect();
    Patched {
        colours: vec![[0.0; 4]; mapping.cells.len()],
        mapping,
        cells,
        colours_gpu,
        slots,
        universes,
        names,
        measured: None,
    }
}

impl Senders {
    /// Opens one sender per universe when the `protocol`, `host` and `port`
    /// parameters change; closes them for `off` and offline (sinks do not
    /// emit in an offline render). `op` names the operator in errors.
    pub fn update(
        &mut self,
        p: &Params<'_>,
        op: &str,
        universes: &[(u16, usize)],
    ) -> Result<(), String> {
        let protocol = if p.mode().is_offline() {
            "off"
        } else {
            p.str("protocol")
        };
        let host = p.str("host");
        let port = p.i64("port").clamp(0, 65_535);
        // Compared in place: steady-state cooks must not allocate (rule 2).
        let same = self.config.as_ref().is_some_and(|(pr, h, po, us)| {
            pr == protocol
                && h == host
                && *po == port
                && us.len() == universes.len()
                && us.iter().zip(universes).all(|(a, (b, _))| a == b)
        });
        if same {
            return Ok(());
        }
        self.senders.clear();
        self.config = Some((
            protocol.to_owned(),
            host.to_owned(),
            port,
            universes.iter().map(|(u, _)| *u).collect(),
        ));
        let protocol = match protocol {
            "artnet" => Protocol::ArtNet,
            "sacn" => Protocol::Sacn,
            _ => return Ok(()),
        };
        let port = (port > 0).then_some(port as u16);
        for &(u, _) in universes {
            self.senders.push(
                DmxSender::new(protocol, host, port, u)
                    .map_err(|e| format!("{op}, universe {u}: {e}"))?,
            );
        }
        Ok(())
    }

    /// Hands each universe's latest slots to its sender (when sending).
    pub fn publish(&self, universes: &[Vec<u8>]) {
        for (sender, data) in self.senders.iter().zip(universes) {
            sender.publish(data);
        }
    }
}

impl PixelMap {
    /// Picks up new versions of the patch file from the watcher.
    fn watch_patch(&mut self, ctx: &mut ImgToSigCtx<'_>) {
        let p = ctx.params;
        let file = p.str("patch");
        if file.is_empty() {
            self.watcher = None;
            self.patch = None;
            self.patch_error = None;
            return;
        }
        if self.watcher.as_ref().is_none_or(|w| w.path != file) {
            let fixtures = p.resolve("fixtures");
            self.watcher = Some(watch(
                file.to_owned(),
                p.resolve(file),
                "atd-pixel-map-patch",
                move |text| {
                    let patch = atd_io::fixture::load_patch(text, &fixtures)?;
                    Mapping::new(&patch)
                },
            ));
            self.patch = None;
            self.patch_error = None;
        }
        let Some(w) = &self.watcher else { return };
        let mode = p.mode();
        let mut latest = None;
        if mode.is_offline() {
            // Offline the file is read once, as it was when the render started.
            if self.patch.is_none() && self.patch_error.is_none() {
                latest = Some(
                    mode.receive(&w.rx)
                        .unwrap_or_else(|_| Err("patch file watcher stopped".into())),
                );
            }
        } else {
            while let Ok(r) = w.rx.try_recv() {
                latest = Some(r);
            }
            // Keep looking for saves.
            ctx.cook_again = true;
        }
        match latest {
            Some(Ok(mapping)) => {
                self.patch = Some(upload(ctx.device, ctx.queue, mapping));
                self.patch_error = None;
                self.stale = true;
            }
            Some(Err(e)) => {
                self.patch = None;
                self.patch_error = Some(format!("{file}: {e}"));
            }
            None => {}
        }
    }
}

impl ImgToSigOp for PixelMap {
    fn cook(&mut self, ctx: &mut ImgToSigCtx<'_>) -> Result<(), String> {
        if let Build::Building(rx) = &self.build {
            match ctx.params.mode().receive(rx) {
                Ok(Ok(p)) => self.build = Build::Ready(p),
                Ok(Err(e)) => self.build = Build::Failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.cook_again = true;
                    return Ok(());
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.build = Build::Failed("Pixel Map build thread stopped".into());
                }
            }
        }
        if let Build::Failed(e) = &self.build {
            return Err(e.clone());
        }
        self.watch_patch(ctx);
        let Build::Ready(pipeline) = &self.build else {
            unreachable!("handled above")
        };
        if let Some(e) = &self.patch_error {
            ctx.output.set_single(&[], &[]);
            return Err(e.clone());
        }
        let Some(patch) = &mut self.patch else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        self.senders
            .update(&ctx.params, "Pixel Map", &patch.mapping.universes)?;

        // Read finished measurements without waiting; offline, wait for
        // the previous frame's, so values are exactly one frame late.
        let poll = if ctx.params.mode().is_offline() {
            wgpu::PollType::wait_indefinitely()
        } else {
            wgpu::PollType::Poll
        };
        let _ = ctx.device.poll(poll);
        let mut fresh = false;
        for slot in &mut patch.slots {
            if slot.state.load(Ordering::Acquire) != READY {
                continue;
            }
            if patch.measured.is_none_or(|m| slot.frame >= m)
                && let Ok(data) = slot.buffer.slice(..).get_mapped_range()
            {
                for (c, b) in patch.colours.iter_mut().zip(data.chunks_exact(16)) {
                    for (v, f) in c.iter_mut().zip(b.chunks_exact(4)) {
                        *v = f32::from_le_bytes([f[0], f[1], f[2], f[3]]);
                    }
                }
                patch.measured = Some(slot.frame);
                fresh = true;
            }
            slot.buffer.unmap();
            slot.state.store(IDLE, Ordering::Release);
        }
        if fresh {
            patch.mapping.fill(&patch.colours, &mut patch.universes);
            self.senders.publish(&patch.universes);
        }

        // Sample when the image may have changed.
        let input = ctx.inputs.first().cloned().flatten();
        let changed = self.stale || ctx.inputs_cooked || input != self.last_input;
        self.stale = false;
        let n = patch.mapping.cells.len() as u32;
        if changed
            && n > 0
            && let Some(texture) = &input
            && let Some(slot) = patch
                .slots
                .iter_mut()
                .find(|s| s.state.load(Ordering::Acquire) == IDLE)
        {
            let view = texture.create_view(&Default::default());
            let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("img.pixel_map"),
                layout: &pipeline.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: patch.cells.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: patch.colours_gpu.as_entire_binding(),
                    },
                ],
            });
            {
                let mut pass = ctx
                    .encoder
                    .begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("img.pixel_map"),
                        timestamp_writes: None,
                    });
                pass.set_bind_group(0, &bind, &[]);
                pass.set_pipeline(&pipeline.pipeline);
                pass.dispatch_workgroups(n.div_ceil(64), 1, 1);
            }
            ctx.encoder.copy_buffer_to_buffer(
                &patch.colours_gpu,
                0,
                &slot.buffer,
                0,
                u64::from(n) * 16,
            );
            slot.frame = ctx.frame;
            slot.since = std::time::Instant::now();
            slot.state.store(RECORDED, Ordering::Release);
            self.last_input = input;
        } else if changed && input.is_some() && n > 0 {
            self.stale = true;
        }

        ctx.cook_again |= self.stale
            || patch
                .slots
                .iter()
                .any(|s| s.state.load(Ordering::Acquire) != IDLE);
        let only = ctx.params.i64("universe");
        let mut w = ctx.output.writer();
        for ((data, names), (u, _)) in patch
            .universes
            .iter()
            .zip(&patch.names)
            .zip(&patch.mapping.universes)
        {
            if only != 0 && i64::from(*u) != only {
                continue;
            }
            for (v, name) in data.iter().zip(names) {
                w.push(name, f32::from(*v) / 255.0);
            }
        }
        drop(w);
        if let Some(slot) = patch
            .slots
            .iter()
            .find(|s| s.state.load(Ordering::Acquire) != IDLE && s.since.elapsed() > STUCK)
        {
            return Err(format!(
                "GPU readback has not completed for {:.1} s",
                slot.since.elapsed().as_secs_f32()
            ));
        }
        Ok(())
    }

    fn after_submit(&mut self) {
        let Some(patch) = &self.patch else { return };
        for slot in &patch.slots {
            if slot.state.load(Ordering::Acquire) == RECORDED {
                slot.state.store(MAPPING, Ordering::Release);
                let state = slot.state.clone();
                slot.buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        state.store(if r.is_ok() { READY } else { IDLE }, Ordering::Release);
                    });
            }
        }
    }
}
