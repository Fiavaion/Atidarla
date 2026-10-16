//! ISF shaders as IMG operators (FR-K6).
//!
//! [`IsfDef`] couples a translated shader ([`atd_gpu::isf::IsfShader`]) with
//! its operator schema. The ISF inputs map as follows:
//!
//! - `float`, `long`, `bool`, `event`, `color` and `point2D` become
//!   parameters;
//! - `image`, `audio` and `audioFFT` become IMG input sockets.
//!
//! [`IsfOp`] runs the passes. Each pass target is a pair of `Rgba16Float`
//! textures, written in turn: a pass that samples its own target reads the
//! previous contents, so persistent feedback buffers work. Non-persistent
//! targets are cleared at the start of every frame. Pass sizes are
//! re-evaluated from their expressions every frame.

use std::path::Path;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp, OpLibrary, Params};
use atd_gpu::fullscreen::FullscreenPass;
use atd_gpu::isf::{
    self, FIRST_TEXTURE_BINDING, IsfError, IsfInputKind, IsfShader, SAMPLER_BINDING, SizeExpr,
    TextureSource, UNIFORM_BINDING, UniformType, VERTEX_COUNT,
};

use crate::stateful::{PingPong, STATE, clear};

/// An imported ISF shader: schema plus translated source.
#[derive(Debug, Clone)]
pub struct IsfDef {
    pub schema: OperatorSchema,
    pub shader: IsfShader,
    /// Uses `TIME`, `TIMEDELTA`, `FRAMEINDEX`, `DATE` or persistent buffers.
    pub time_dependent: bool,
    /// Directory `IMPORTED` image paths are relative to.
    pub dir: Option<std::path::PathBuf>,
}

/// Operator type ID for a shader file stem: `img.isf_<slug>`.
pub fn type_id_for(stem: &str) -> String {
    let mut slug = String::from("img.isf_");
    let mut last_underscore = true;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            slug.push('_');
            last_underscore = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    slug
}

impl IsfDef {
    /// Translates an ISF shader. `stem` names the operator (usually the file
    /// stem); `vs` is the optional `.vs` vertex shader.
    pub fn new(stem: &str, fs: &str, vs: Option<&str>) -> Result<Self, IsfError> {
        let shader = isf::translate(fs, vs)?;
        let header = &shader.header;
        let uses = |t: &str| fs.contains(t) || vs.is_some_and(|v| v.contains(t));
        let time_dependent = ["TIME", "FRAMEINDEX", "DATE"].iter().any(|t| uses(t))
            || header.passes.iter().any(|p| p.persistent);

        let mut params = Vec::new();
        let mut inputs = Vec::new();
        for input in &header.inputs {
            let label = input.label.clone().unwrap_or_else(|| input.name.clone());
            let help = format!("ISF input `{}`.", input.name);
            let n = input.name.as_str();
            let param = match &input.kind {
                IsfInputKind::Float { min, max, default } => Some(ParamSchema::float(
                    n,
                    &label,
                    f64::from(*default),
                    Some(f64::from(min.min(*default))),
                    Some(f64::from(max.max(*default))),
                )),
                IsfInputKind::Long {
                    min,
                    max,
                    default,
                    values,
                    labels,
                } => Some(if !labels.is_empty() && labels.len() == values.len() {
                    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let default = values
                        .iter()
                        .position(|v| v == default)
                        .map_or(options[0], |i| options[i]);
                    ParamSchema::menu(n, &label, &options, default)
                } else {
                    ParamSchema::int(
                        n,
                        &label,
                        i64::from(*default),
                        Some(i64::from((*min).min(*default))),
                        Some(i64::from((*max).max(*default))),
                    )
                }),
                IsfInputKind::Bool { default } => Some(ParamSchema::toggle(n, &label, *default)),
                IsfInputKind::Event => Some(ParamSchema::pulse(n, &label)),
                IsfInputKind::Color { default } => {
                    Some(ParamSchema::colour(n, &label, default.map(f64::from)))
                }
                IsfInputKind::Point2D { default, .. } => {
                    Some(ParamSchema::vec(n, &label, &default.map(f64::from)))
                }
                IsfInputKind::Image
                | IsfInputKind::Audio { .. }
                | IsfInputKind::AudioFft { .. } => {
                    inputs.push(SocketSchema {
                        name: input.name.clone(),
                        wire: WireType::Img,
                        optional: true,
                        feedback: false,
                    });
                    None
                }
            };
            if let Some(p) = param {
                params.push(p.help(&help));
            }
        }
        let mut summary = header
            .description
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| format!("ISF shader {stem}."));
        if let Some(credit) = header.credit.as_deref().filter(|c| !c.trim().is_empty()) {
            summary.push_str(&format!(" ({credit})"));
        }
        let passes = header.passes.len() as u32;
        let schema = OperatorSchema {
            type_id: type_id_for(stem),
            family: OpFamily::Img,
            label: stem.to_owned(),
            summary,
            version: 1,
            params,
            inputs,
            outputs: vec![SocketSchema {
                name: "out".into(),
                wire: WireType::Img,
                optional: true,
                feedback: false,
            }],
            capabilities: vec![],
            cook_budget_us: 150 * passes.max(1),
            time_dependent,
            sink: false,
        };
        Ok(Self {
            schema,
            shader,
            time_dependent,
            dir: None,
        })
    }

    /// Sets the directory `IMPORTED` images are loaded from.
    pub fn with_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Creates the operator instance (see [`OpLibrary::add`]).
    pub fn instantiate(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> Box<dyn GpuOp> {
        // Shader compilation can take seconds (FXC on Windows unrolls
        // loops), so it runs on a worker thread, never on the render thread
        // (rule 1). Until it finishes the node outputs transparent black and
        // asks to be cooked again.
        let (tx, rx) = std::sync::mpsc::channel();
        let device = device.clone();
        let label = self.schema.type_id.clone();
        let shader = self.shader.clone();
        let dir = self.dir.clone();
        let spawned = std::thread::Builder::new()
            .name("atd-isf-compile".into())
            .spawn(move || {
                // A panic inside the driver's compiler must not take the
                // reason with it: report it as the node's error.
                let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut op = IsfOp::new(&device, format, &label, &shader)?;
                    op.load_imports(dir.as_deref());
                    Ok(op)
                }))
                .unwrap_or_else(|panic| {
                    let msg = panic
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or("unknown panic");
                    Err(format!("shader compilation panicked: {msg}"))
                });
                let _ = tx.send(built);
            });
        match spawned {
            Ok(_) => Box::new(Compiling::Pending(rx)),
            Err(e) => Box::new(Compiling::Failed(format!(
                "cannot start compiler thread: {e}"
            ))),
        }
    }
}

/// An ISF operator whose pipelines are being built on a worker thread.
enum Compiling {
    Pending(std::sync::mpsc::Receiver<Result<IsfOp, String>>),
    Ready(Box<IsfOp>),
    Failed(String),
}

impl GpuOp for Compiling {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        if let Self::Pending(rx) = self {
            use std::sync::mpsc::TryRecvError;
            *self = match rx.try_recv() {
                Ok(Ok(op)) => Self::Ready(Box::new(op)),
                Ok(Err(e)) => Self::Failed(e),
                Err(TryRecvError::Disconnected) => {
                    Self::Failed("ISF compiler thread failed".into())
                }
                Err(TryRecvError::Empty) => {
                    clear(ctx.encoder, ctx.output);
                    ctx.cook_again = true;
                    return Ok(());
                }
            };
        }
        match self {
            Self::Ready(op) => op.cook(ctx),
            Self::Failed(e) => Err(e.clone()),
            Self::Pending(_) => unreachable!("resolved above"),
        }
    }
}

/// Shaders that translated, and failures by file stem.
pub type Loaded = (Vec<IsfDef>, Vec<(String, IsfError)>);

/// Imports every `.fs` under `dir`. Returns the shaders that translated and
/// the failures by file name.
pub fn load_dir(dir: &Path) -> std::io::Result<Loaded> {
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();
    let (mut ok, mut failed) = (Vec::new(), Vec::new());
    for path in files {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fs = std::fs::read_to_string(&path)?;
        let vs = std::fs::read_to_string(path.with_extension("vs")).ok();
        match IsfDef::new(&stem, &fs, vs.as_deref()) {
            Ok(def) => ok.push(match path.parent() {
                Some(dir) => def.with_dir(dir),
                None => def,
            }),
            Err(e) => failed.push((stem, e)),
        }
    }
    Ok((ok, failed))
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "fs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Registers ISF operators in a registry and library, skipping type IDs
/// that are already taken. Returns the IDs registered.
pub fn register(
    defs: Vec<IsfDef>,
    registry: &mut atd_core::registry::Registry,
    library: &mut OpLibrary,
) -> Vec<String> {
    let mut ids = Vec::new();
    for def in defs {
        let id = def.schema.type_id.clone();
        if registry.register(def.schema.clone()).is_ok() {
            library.add(id.clone(), move |device, format| {
                def.instantiate(device, format)
            });
            ids.push(id);
        }
    }
    ids
}

/// One pass target: a ping-pong pair at the pass's current size.
#[derive(Debug)]
struct Target {
    name: String,
    buffers: Option<PingPong>,
    persistent: bool,
}

/// Runs a translated ISF shader.
#[derive(Debug)]
pub struct IsfOp {
    shader: IsfShader,
    layout: wgpu::BindGroupLayout,
    /// Pipeline writing the operator's output format.
    out_pipeline: wgpu::RenderPipeline,
    /// Pipeline writing pass targets ([`STATE`]).
    target_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Bytes per pass slot in `uniforms` (aligned for binding offsets).
    stride: u64,
    data: Vec<u8>,
    targets: Vec<Target>,
    last_time: Option<f64>,
    frame: i32,
    blit: FullscreenPass,
    /// `IMPORTED` images, by index in the header.
    imports: Vec<Import>,
    /// Per pass: index of its target in `targets`, if it has one.
    pass_target: Vec<Option<usize>>,
    /// Per pass: the first pass that writes its target.
    pass_first: Vec<bool>,
    /// Per pass: width and height expressions, parsed once (`None` when
    /// absent or unparsable: full size).
    pass_exprs: Vec<(Option<SizeExpr>, Option<SizeExpr>)>,
    /// Where each sampled texture comes from, in binding order.
    sources: Vec<Source>,
    /// Per pass: (sampled textures, bind group, last cook), so steady-state
    /// frames create no GPU objects (rule 2).
    binds: Vec<Vec<(Vec<wgpu::Texture>, wgpu::BindGroup, u64)>>,
    /// (render target, view, last cook).
    views: Vec<(wgpu::Texture, wgpu::TextureView, u64)>,
    /// The current pass's sampled textures (reused buffer).
    key: Vec<wgpu::Texture>,
    cooks: u64,
}

/// Bind groups kept per pass.
const CACHE: usize = 8;

/// Cached entries unused for this many cooks are dropped.
const STALE_COOKS: u64 = 16;

/// A sampled texture's origin, resolved once from the header.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// Input socket index.
    Socket(usize),
    /// `IMPORTED` image index.
    Import(usize),
    /// Index into `IsfOp::targets`.
    Target(usize),
}

/// An `IMPORTED` image: decoded on the compile thread, uploaded on the
/// first cook.
#[derive(Debug)]
enum Import {
    Missing,
    Decoded(atd_io::image::Image),
    Uploaded(wgpu::Texture),
}

impl IsfOp {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        label: &str,
        shader: &IsfShader,
    ) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader.vertex_wgsl.as_str().into()),
        });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader.fragment_wgsl.as_str().into()),
        });
        let stages = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let uniform_size = u64::from(shader.uniforms.size.max(16));
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: UNIFORM_BINDING,
                visibility: stages,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: SAMPLER_BINDING,
                visibility: stages,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        for i in 0..shader.textures.len() {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: FIRST_TEXTURE_BINDING + i as u32,
                visibility: stages,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &vs,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &fs,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let out_pipeline = pipeline(format);
        let target_pipeline = pipeline(STATE);
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("ISF pipeline: {err}"));
        }

        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let stride = uniform_size.div_ceil(align) * align;
        let passes = shader.header.passes.len() as u64;
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: stride * passes,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(label),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let targets = shader
            .textures
            .iter()
            .filter_map(|t| match t {
                TextureSource::Target(name) => Some(Target {
                    name: name.clone(),
                    buffers: None,
                    persistent: shader
                        .header
                        .passes
                        .iter()
                        .any(|p| p.target.as_deref() == Some(name) && p.persistent),
                }),
                _ => None,
            })
            .collect();
        let targets: Vec<Target> = targets;
        let target_index = |name: &str| targets.iter().position(|t| t.name == name);
        let pass_target: Vec<Option<usize>> = shader
            .header
            .passes
            .iter()
            .map(|p| p.target.as_deref().and_then(target_index))
            .collect();
        let pass_first = (0..pass_target.len())
            .map(|p| pass_target[p].is_some() && !pass_target[..p].contains(&pass_target[p]))
            .collect();
        let sources = shader
            .textures
            .iter()
            .map(|t| match t {
                TextureSource::Input(i) => Source::Socket(
                    shader.header.inputs[..*i]
                        .iter()
                        .filter(|x| x.kind.is_texture())
                        .count(),
                ),
                TextureSource::Imported(i) => Source::Import(*i),
                TextureSource::Target(name) => {
                    Source::Target(target_index(name).expect("every sampled target is known"))
                }
            })
            .collect();
        let parse = |e: &Option<String>| e.as_deref().and_then(|e| SizeExpr::parse(e).ok());
        let pass_exprs = shader
            .header
            .passes
            .iter()
            .map(|p| (parse(&p.width), parse(&p.height)))
            .collect();
        let passes = passes as usize;
        Ok(Self {
            shader: shader.clone(),
            layout,
            out_pipeline,
            target_pipeline,
            sampler,
            uniforms,
            stride,
            data: vec![0; stride as usize * passes],
            targets,
            last_time: None,
            frame: 0,
            blit: FullscreenPass::new(
                device,
                "atd-isf-blit",
                format,
                1,
                "",
                "return textureSample(in0, samp, uv);",
            ),
            imports: Vec::new(),
            pass_target,
            pass_first,
            pass_exprs,
            sources,
            binds: (0..passes).map(|_| Vec::with_capacity(CACHE)).collect(),
            views: Vec::with_capacity(2 * CACHE),
            key: Vec::with_capacity(shader.textures.len()),
            cooks: 0,
        })
    }

    /// Decodes the header's `IMPORTED` images from `dir` (missing or
    /// unreadable images sample as black, with a warning).
    pub fn load_imports(&mut self, dir: Option<&std::path::Path>) {
        self.imports = self
            .shader
            .header
            .imported
            .iter()
            .map(|(name, path)| {
                let Some(dir) = dir else {
                    return Import::Missing;
                };
                match atd_io::image::load(&dir.join(path)) {
                    Ok(image) => Import::Decoded(image),
                    Err(e) => {
                        log::warn!("ISF image {name}: {e}");
                        Import::Missing
                    }
                }
            })
            .collect();
    }

    /// Uploads decoded imports (once).
    fn upload_imports(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        for import in &mut self.imports {
            if let Import::Decoded(image) = import {
                let size = wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                };
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("atd-isf-import"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &image.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(image.width * 4),
                        rows_per_image: Some(image.height),
                    },
                    size,
                );
                *import = Import::Uploaded(texture);
            }
        }
    }

    /// Writes the uniforms for pass `pass` (render size `size`) into `data`.
    fn pack(&mut self, pass: usize, size: [f32; 2], params: &Params<'_>, time: f64, dt: f64) {
        let base = pass * self.stride as usize;
        let date = date_now();
        for field in &self.shader.uniforms.fields {
            let at = base + field.offset as usize;
            let floats: [f32; 4] = match field.name.as_str() {
                "DATE" => date,
                "RENDERSIZE" => [size[0], size[1], 0.0, 0.0],
                "TIME" => [time as f32, 0.0, 0.0, 0.0],
                "TIMEDELTA" => [dt as f32, 0.0, 0.0, 0.0],
                "PASSINDEX" => [f32::from_bits(pass as u32), 0.0, 0.0, 0.0],
                "FRAMEINDEX" => [f32::from_bits(self.frame as u32), 0.0, 0.0, 0.0],
                name => input_value(&self.shader, name, field.ty, params),
            };
            let n = (field.ty.size() / 4) as usize;
            for (i, f) in floats[..n].iter().enumerate() {
                self.data[at + i * 4..at + i * 4 + 4].copy_from_slice(&f.to_le_bytes());
            }
        }
    }

    /// Size of pass `p`'s render target for an output of `w`×`h`.
    fn pass_size(&self, p: usize, w: u32, h: u32, params: &Params<'_>) -> (u32, u32) {
        let (width, height) = &self.pass_exprs[p];
        let vars = |name: &str| -> Option<f64> {
            match name {
                "WIDTH" => Some(f64::from(w)),
                "HEIGHT" => Some(f64::from(h)),
                other => Some(f64::from(
                    input_value(&self.shader, other, UniformType::Float, params)[0],
                )),
            }
        };
        let eval = |e: &Option<SizeExpr>, full: u32| {
            e.as_ref()
                .and_then(|e| e.eval(&vars).ok())
                .filter(|v| v.is_finite())
                .map_or(full, |v| v.clamp(1.0, 8192.0) as u32)
        };
        (eval(width, w), eval(height, h))
    }
}

/// The uniform value of ISF input `name`, as raw 32-bit lanes.
fn input_value(shader: &IsfShader, name: &str, ty: UniformType, params: &Params<'_>) -> [f32; 4] {
    let Some(input) = shader.header.inputs.iter().find(|i| i.name == name) else {
        return [0.0; 4];
    };
    let int = |v: i32| [f32::from_bits(v as u32), 0.0, 0.0, 0.0];
    let as_float = ty == UniformType::Float;
    match &input.kind {
        IsfInputKind::Float { .. } => [params.f32(name), 0.0, 0.0, 0.0],
        IsfInputKind::Long { values, labels, .. } => {
            let v = if !labels.is_empty() && labels.len() == values.len() {
                let label = params.str(name);
                labels
                    .iter()
                    .position(|l| l == label)
                    .map_or(values[0], |i| values[i])
            } else {
                params.i64(name) as i32
            };
            if as_float {
                [v as f32, 0.0, 0.0, 0.0]
            } else {
                int(v)
            }
        }
        IsfInputKind::Bool { .. } | IsfInputKind::Event => {
            let v = i32::from(params.bool(name));
            if as_float {
                [v as f32, 0.0, 0.0, 0.0]
            } else {
                int(v)
            }
        }
        IsfInputKind::Color { .. } => params.vec4(name),
        IsfInputKind::Point2D { .. } => {
            let v = params.vec4(name);
            [v[0], v[1], 0.0, 0.0]
        }
        _ => [0.0; 4],
    }
}

/// ISF `DATE`: year, month (1–12), day (1–31), seconds since midnight (UTC).
fn date_now() -> [f32; 4] {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let days = (secs / 86_400.0).floor() as i64;
    let (y, m, d) = civil_from_days(days);
    [
        y as f32,
        m as f32,
        d as f32,
        (secs - days as f64 * 86_400.0) as f32,
    ]
}

/// Proleptic Gregorian date from days since 1970-01-01 (H. Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

impl GpuOp for IsfOp {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        self.upload_imports(ctx.device, ctx.queue);
        let (w, h) = (ctx.output.width(), ctx.output.height());
        let dt = self.last_time.map_or(0.0, |t| (ctx.time - t).max(0.0));
        self.last_time = Some(ctx.time);
        let passes = self.shader.header.passes.len();
        self.cooks += 1;
        let now = self.cooks;

        // Size targets; (re)create on change, clear non-persistent ones.
        for p in 0..passes {
            let Some(ti) = self.pass_target[p] else {
                continue;
            };
            let (tw, th) = self.pass_size(p, w, h, &ctx.params);
            let first = self.pass_first[p];
            let t = &mut self.targets[ti];
            let fresh = t
                .buffers
                .as_ref()
                .is_none_or(|b| b.read().width() != tw || b.read().height() != th);
            if fresh {
                let b = PingPong::new(ctx.device, tw, th);
                clear(ctx.encoder, b.read());
                clear(ctx.encoder, b.write());
                t.buffers = Some(b);
            } else if !t.persistent && first {
                let b = t.buffers.as_ref().expect("checked above");
                clear(ctx.encoder, b.read());
            }
        }

        // Cached bind groups and views hold their textures: drop those not
        // used lately (in place, no allocation).
        for binds in &mut self.binds {
            binds.retain(|e| now - e.2 <= STALE_COOKS);
        }
        self.views.retain(|e| now - e.2 <= STALE_COOKS);

        // Every pass's uniforms, written in one go.
        for p in 0..passes {
            let size = match self.pass_target[p] {
                Some(ti) => {
                    let b = self.targets[ti].buffers.as_ref().expect("sized above");
                    [b.read().width() as f32, b.read().height() as f32]
                }
                None => [w as f32, h as f32],
            };
            self.pack(p, size, &ctx.params, ctx.time, dt);
        }
        ctx.queue.write_buffer(&self.uniforms, 0, &self.data);

        for p in 0..passes {
            let at = p as u64 * self.stride;

            // The textures this pass samples, as the cache key.
            self.key.clear();
            for source in &self.sources {
                let tex = match *source {
                    Source::Socket(i) => ctx.input_or_black(i),
                    Source::Import(i) => match self.imports.get(i) {
                        Some(Import::Uploaded(t)) => t,
                        _ => ctx.black,
                    },
                    Source::Target(i) => self.targets[i]
                        .buffers
                        .as_ref()
                        .map_or(ctx.black, PingPong::read),
                };
                self.key.push(tex.clone());
            }
            let binds = &mut self.binds[p];
            let bind = match binds.iter().position(|(k, _, _)| *k == self.key) {
                Some(i) => {
                    binds[i].2 = now;
                    &binds[i].1
                }
                None => {
                    let views: Vec<wgpu::TextureView> = self
                        .key
                        .iter()
                        .map(|t| t.create_view(&Default::default()))
                        .collect();
                    let mut entries = vec![
                        wgpu::BindGroupEntry {
                            binding: UNIFORM_BINDING,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.uniforms,
                                offset: at,
                                size: wgpu::BufferSize::new(u64::from(
                                    self.shader.uniforms.size.max(16),
                                )),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: SAMPLER_BINDING,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ];
                    for (i, view) in views.iter().enumerate() {
                        entries.push(wgpu::BindGroupEntry {
                            binding: FIRST_TEXTURE_BINDING + i as u32,
                            resource: wgpu::BindingResource::TextureView(view),
                        });
                    }
                    let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &self.layout,
                        entries: &entries,
                    });
                    if binds.len() == CACHE {
                        binds.remove(0);
                    }
                    binds.push((self.key.clone(), bind, now));
                    &binds.last().expect("pushed").1
                }
            };
            let (dest, pipeline) = match self.pass_target[p] {
                Some(ti) => (
                    self.targets[ti].buffers.as_ref().expect("sized").write(),
                    &self.target_pipeline,
                ),
                None => (ctx.output, &self.out_pipeline),
            };
            let view = match self.views.iter().position(|(t, _, _)| t == dest) {
                Some(i) => {
                    self.views[i].2 = now;
                    &self.views[i].1
                }
                None => {
                    if self.views.len() == 2 * CACHE {
                        self.views.remove(0);
                    }
                    let view = dest.create_view(&Default::default());
                    self.views.push((dest.clone(), view, now));
                    &self.views.last().expect("pushed").1
                }
            };
            {
                let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("atd-isf"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..VERTEX_COUNT, 0..1);
            }
            if let Some(ti) = self.pass_target[p] {
                self.targets[ti].buffers.as_mut().expect("sized").swap();
            }
        }
        // A final pass with a target is also the output.
        if let Some(ti) = self.pass_target[passes - 1] {
            let src = self.targets[ti].buffers.as_ref().expect("sized").read();
            self.blit
                .draw(ctx.device, ctx.queue, ctx.encoder, &[src], &[], ctx.output);
        }
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_ids_are_slugs() {
        assert_eq!(type_id_for("Bad TV"), "img.isf_bad_tv");
        assert_eq!(type_id_for("v002-CRT-Mask"), "img.isf_v002_crt_mask");
        assert_eq!(type_id_for("  3d Rotate!"), "img.isf_3d_rotate");
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_741), (2026, 10, 15));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
