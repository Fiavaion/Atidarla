//! Analyze (PRD §6, IMG render & code → SIG): average colour, luminance and
//! its range over the whole image, as channels.
//!
//! Two compute passes reduce the image on the GPU (64 workgroups of partial
//! sums, then one that combines them). The 32-byte result is copied into one
//! of three readback buffers, mapped after the frame is submitted, and read
//! on a later cook, so the render thread never waits for the GPU (rule 1).
//! Channels therefore trail the image by a frame or two.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use atd_core::registry::{OpFamily, OperatorSchema, SocketSchema, WireType};
use atd_gpu::engine::{GpuToSigCtx, GpuToSigOp};

const NAMES: [&str; 7] = ["r", "g", "b", "a", "luma", "min", "max"];
/// Workgroups in the first pass.
const GROUPS: u32 = 64;
const RESULT_BYTES: u64 = 8 * 4;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.analyze".into(),
        family: OpFamily::Img,
        label: "Analyze".into(),
        summary: "Measures the whole image on the GPU: average r, g, b, a and luminance, and the darkest and brightest luminance, as channels (a frame or two behind the image).".into(),
        version: 1,
        params: vec![],
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
        cook_budget_us: 60,
        time_dependent: false,
        sink: false,
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> partial: array<vec4f, 128>;
@group(0) @binding(2) var<storage, read_write> result: array<vec4f, 2>;

var<workgroup> s_sum: array<vec4f, 256>;
var<workgroup> s_lum: array<vec4f, 256>;

const LUMA = vec3f(0.2126, 0.7152, 0.0722);

// Pass 1: each workgroup sums a strided share of the pixels.
// partial[2g] = rgba sum, partial[2g+1] = (luma sum, min, max, count).
@compute @workgroup_size(256)
fn reduce(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3u) {
    let dims = textureDimensions(src);
    let n = dims.x * dims.y;
    let stride = 256u * 64u;
    var sum = vec4f(0.0);
    var lum = vec4f(0.0, 1e30, -1e30, 0.0);
    for (var i = wg.x * 256u + li; i < n; i += stride) {
        let c = textureLoad(src, vec2u(i % dims.x, i / dims.x), 0);
        let l = dot(c.rgb, LUMA);
        sum += c;
        lum = vec4f(lum.x + l, min(lum.y, l), max(lum.z, l), lum.w + 1.0);
    }
    s_sum[li] = sum;
    s_lum[li] = lum;
    workgroupBarrier();
    for (var k = 128u; k > 0u; k >>= 1u) {
        if (li < k) {
            s_sum[li] += s_sum[li + k];
            let a = s_lum[li];
            let b = s_lum[li + k];
            s_lum[li] = vec4f(a.x + b.x, min(a.y, b.y), max(a.z, b.z), a.w + b.w);
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        partial[2u * wg.x] = s_sum[0];
        partial[2u * wg.x + 1u] = s_lum[0];
    }
}

// Pass 2: one workgroup combines the 64 partials into averages.
@compute @workgroup_size(64)
fn combine(@builtin(local_invocation_index) li: u32) {
    s_sum[li] = partial[2u * li];
    s_lum[li] = partial[2u * li + 1u];
    workgroupBarrier();
    for (var k = 32u; k > 0u; k >>= 1u) {
        if (li < k) {
            s_sum[li] += s_sum[li + k];
            let a = s_lum[li];
            let b = s_lum[li + k];
            s_lum[li] = vec4f(a.x + b.x, min(a.y, b.y), max(a.z, b.z), a.w + b.w);
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        let count = max(s_lum[0].w, 1.0);
        result[0] = s_sum[0] / count;
        result[1] = vec4f(s_lum[0].x / count, s_lum[0].y, s_lum[0].z, count);
    }
}
"#;

const IDLE: u8 = 0;
const RECORDED: u8 = 1;
const MAPPING: u8 = 2;
const READY: u8 = 3;

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// Engine frame the measurement was recorded in, and when.
    frame: u64,
    since: std::time::Instant,
}

/// A readback taking longer than this is reported as an error.
const STUCK: std::time::Duration = std::time::Duration::from_secs(5);

pub struct Analyze {
    reduce: wgpu::ComputePipeline,
    combine: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    partial: wgpu::Buffer,
    result: wgpu::Buffer,
    slots: Vec<Slot>,
    /// Latest values and the frame they were measured in.
    values: [f32; 7],
    measured: Option<u64>,
    /// The input image last measured.
    last_input: Option<wgpu::Texture>,
    /// The image changed while every readback slot was busy.
    stale: bool,
}

impl std::fmt::Debug for Analyze {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analyze")
            .field("values", &self.values)
            .field("measured", &self.measured)
            .finish_non_exhaustive()
    }
}

/// The operator: pipelines are built on a worker thread (shader compilers
/// such as FXC can take seconds, rule 1); until then it outputs zeros.
enum AnalyzeOp {
    Building(std::sync::mpsc::Receiver<Result<Analyze, String>>),
    Ready(Box<Analyze>),
    Failed(String),
}

pub fn create(device: &wgpu::Device) -> Box<dyn GpuToSigOp> {
    let (tx, rx) = std::sync::mpsc::channel();
    let device = device.clone();
    let spawned = std::thread::Builder::new()
        .name("atd-analyze-build".into())
        .spawn(move || {
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let op = build(&device);
                match pollster::block_on(scope.pop()) {
                    Some(e) => Err(format!("Analyze pipelines: {e}")),
                    None => Ok(op),
                }
            }))
            .unwrap_or_else(|_| Err("Analyze pipeline build panicked".into()));
            let _ = tx.send(built);
        });
    Box::new(match spawned {
        Ok(_) => AnalyzeOp::Building(rx),
        Err(e) => AnalyzeOp::Failed(format!("cannot start build thread: {e}")),
    })
}

impl GpuToSigOp for AnalyzeOp {
    fn cook(&mut self, ctx: &mut GpuToSigCtx<'_>) -> Result<(), String> {
        if let AnalyzeOp::Building(rx) = self {
            // Offline the first cook waits for the pipelines (FR-H1).
            match ctx.params.mode().receive(rx) {
                Ok(Ok(op)) => *self = AnalyzeOp::Ready(Box::new(op)),
                Ok(Err(e)) => *self = AnalyzeOp::Failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.cook_again = true;
                    ctx.output.set_single(&NAMES, &[0.0; 7]);
                    return Ok(());
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    *self = AnalyzeOp::Failed("Analyze build thread stopped".into());
                }
            }
        }
        match self {
            AnalyzeOp::Ready(op) => op.cook(ctx),
            AnalyzeOp::Failed(e) => {
                ctx.output.set_single(&NAMES, &[0.0; 7]);
                Err(e.clone())
            }
            AnalyzeOp::Building(_) => unreachable!("handled above"),
        }
    }

    fn after_submit(&mut self) {
        if let AnalyzeOp::Ready(op) = self {
            op.after_submit();
        }
    }
}

fn build(device: &wgpu::Device) -> Analyze {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("img.analyze"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let storage = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("img.analyze"),
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
            storage(1),
            storage(2),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("img.analyze"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("img.analyze"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let buffer = |size: u64, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.analyze"),
            size,
            usage,
            mapped_at_creation: false,
        })
    };
    let slots = (0..3)
        .map(|_| Slot {
            buffer: buffer(
                RESULT_BYTES,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            state: Arc::new(AtomicU8::new(IDLE)),
            frame: 0,
            since: std::time::Instant::now(),
        })
        .collect();
    Analyze {
        reduce: pipeline("reduce"),
        combine: pipeline("combine"),
        layout,
        partial: buffer(u64::from(GROUPS) * 32, wgpu::BufferUsages::STORAGE),
        result: buffer(
            RESULT_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        ),
        slots,
        values: [0.0; 7],
        measured: None,
        last_input: None,
        stale: false,
    }
}

impl Analyze {
    fn cook(&mut self, ctx: &mut GpuToSigCtx<'_>) -> Result<(), String> {
        // Let finished maps report, without waiting. Offline, wait for the
        // previous frame's readback, so the values are always exactly one
        // frame late (FR-H1).
        let poll = if ctx.params.mode().is_offline() {
            wgpu::PollType::wait_indefinitely()
        } else {
            wgpu::PollType::Poll
        };
        let _ = ctx.device.poll(poll);
        for slot in &mut self.slots {
            if slot.state.load(Ordering::Acquire) != READY {
                continue;
            }
            if self.measured.is_none_or(|m| slot.frame >= m)
                && let Ok(data) = slot.buffer.slice(..).get_mapped_range()
            {
                for (v, b) in self.values.iter_mut().zip(data.chunks_exact(4)) {
                    *v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                }
                self.measured = Some(slot.frame);
            }
            slot.buffer.unmap();
            slot.state.store(IDLE, Ordering::Release);
        }

        // Measure when the image may have changed: its source cooked this
        // frame, or a different texture is connected.
        let input = ctx.inputs.first().cloned().flatten();
        let changed = self.stale || ctx.inputs_cooked || input != self.last_input;
        self.stale = false;
        if changed
            && let Some(texture) = &input
            && let Some(slot) = self
                .slots
                .iter_mut()
                .find(|s| s.state.load(Ordering::Acquire) == IDLE)
        {
            let view = texture.create_view(&Default::default());
            let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("img.analyze"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.partial.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.result.as_entire_binding(),
                    },
                ],
            });
            {
                let mut pass = ctx
                    .encoder
                    .begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("img.analyze"),
                        timestamp_writes: None,
                    });
                pass.set_bind_group(0, &bind, &[]);
                pass.set_pipeline(&self.reduce);
                pass.dispatch_workgroups(GROUPS, 1, 1);
                pass.set_pipeline(&self.combine);
                pass.dispatch_workgroups(1, 1, 1);
            }
            ctx.encoder
                .copy_buffer_to_buffer(&self.result, 0, &slot.buffer, 0, RESULT_BYTES);
            slot.frame = ctx.frame;
            slot.since = std::time::Instant::now();
            slot.state.store(RECORDED, Ordering::Release);
            self.last_input = input;
        } else if changed && input.is_some() {
            // No free slot: measure as soon as one frees.
            self.stale = true;
        }

        // Come back while measurements are in flight.
        ctx.cook_again = self.stale
            || self
                .slots
                .iter()
                .any(|s| s.state.load(Ordering::Acquire) != IDLE);
        ctx.output.set_single(&NAMES, &self.values);
        if let Some(slot) = self
            .slots
            .iter()
            .find(|s| s.state.load(Ordering::Acquire) != IDLE && s.since.elapsed() > STUCK)
        {
            let states: Vec<u8> = self
                .slots
                .iter()
                .map(|s| s.state.load(Ordering::Acquire))
                .collect();
            return Err(format!(
                "GPU readback has not completed for {:.1} s (slot states {states:?}: 1 recorded, 2 mapping, 3 ready)",
                slot.since.elapsed().as_secs_f32()
            ));
        }
        Ok(())
    }

    fn after_submit(&mut self) {
        for slot in &self.slots {
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
