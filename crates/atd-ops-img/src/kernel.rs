//! Kernel DSL operators shared by both targets (A12, `docs/kernel-dsl.md`):
//! where a kernel's source comes from ([`KernelSource`] for the inline
//! `code`/`file` parameters, [`SharedSource`] for a project's `kernels/`
//! file), where its `ch()` values come from ([`ChValues`]), and the Pixels
//! runner behind `img.kernel` and the project's `.pixels.atk` operators.
//!
//! Compiling (the `atd-kernel` front end, naga, then the driver) runs on a
//! worker thread and a file is read by its own thread, so neither touches the
//! render thread (rule 1). The last kernel that compiled keeps running while a
//! new one compiles and when it fails.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp, Params};
use atd_gpu::fullscreen::{FullscreenPass, ModuleLayout};
use atd_kernel::{CompileOptions, Compiled, FrameGlobals, ParamDecl, ParamType, Target};

use crate::stateful::clear;

/// Input images of a Pixels kernel (`texture(0..3, uv)`).
pub const INPUTS: usize = 4;

const DEFAULT: &str =
    "// Runs once for every pixel. @Cd starts as input 0 here; @P is the pixel's uv.
float speed = ch(\"speed\", 1.0, 0.0, 10.0);
vec3 wave = set(@P.x, @P.y, 0.5 + 0.5 * sin(@Time * speed));
@Cd = vec4(lerp(@Cd.rgb, wave, 0.5), 1);
";

/// A kernel's text, or why it cannot be read.
pub type Text = Result<Arc<str>, String>;

/// Where a kernel's `ch()` parameters get their values.
#[derive(Debug, Clone, Copy)]
pub enum ChValues<'a> {
    /// Their defaults: the inline `pnt.kernel` and `img.kernel`, whose
    /// schemas have no parameters for them.
    Defaults,
    /// The node's parameters of the same names (a project kernel operator),
    /// with bindings, expressions and live values; the default where the
    /// node has no such parameter yet.
    Params(Params<'a>),
}

/// One parameter's value for the uniform buffer (no allocation).
fn value(params: &Params<'_>, d: &ParamDecl) -> [f32; 4] {
    let set = params.value(&d.name).is_some()
        || params.bound(&d.name).is_some()
        || params.expr(&d.name).is_some();
    if !set {
        return d.default;
    }
    match d.ty {
        ParamType::Float => [params.f32(&d.name), 0.0, 0.0, 0.0],
        ParamType::Int => [params.i64(&d.name) as f32, 0.0, 0.0, 0.0],
        ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4 => params.vec4(&d.name),
    }
}

/// Fills `out[slot]` for every parameter of `compiled` (`out` has one entry
/// per parameter).
pub fn fill_values(compiled: &Compiled, source: ChValues<'_>, out: &mut [[f32; 4]]) {
    for d in &compiled.params {
        if let Some(slot) = out.get_mut(d.slot as usize) {
            *slot = match source {
                ChValues::Defaults => d.default,
                ChValues::Params(p) => value(&p, d),
            };
        }
    }
}

/// Each parameter's default, by slot.
pub fn defaults(compiled: &Compiled) -> Vec<[f32; 4]> {
    let mut values = vec![[0.0; 4]; compiled.params.len()];
    fill_values(compiled, ChValues::Defaults, &mut values);
    values
}

/// A project kernel file's latest text, shared by the project (which reads
/// the file when it is saved) and the nodes of its operator.
#[derive(Debug)]
pub struct SharedSource {
    generation: AtomicU64,
    text: Mutex<Text>,
}

impl SharedSource {
    pub fn new(text: Text) -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU64::new(1),
            text: Mutex::new(text),
        })
    }

    /// Replaces the text; nodes pick it up at their next cook.
    pub fn set(&self, text: Text) {
        *self.text.lock().unwrap_or_else(|e| e.into_inner()) = text;
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn get(&self) -> Text {
        self.text.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The text when it changed since generation `seen` (which is updated).
    /// Clones an `Arc` only when it changed.
    pub fn newer(&self, seen: &mut u64) -> Option<Text> {
        let now = self.generation.load(Ordering::Acquire);
        if now == *seen {
            return None;
        }
        *seen = now;
        Some(self.get())
    }
}

/// Reads a kernel file whenever it changes.
#[derive(Debug)]
struct Watcher {
    path: String,
    stop: Arc<AtomicBool>,
    rx: Receiver<Text>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(path: String, file: std::path::PathBuf) -> Watcher {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let label = path.clone();
    let spawned = std::thread::Builder::new()
        .name("atd-kernel-watch".into())
        .spawn(move || {
            let mut last: Option<Text> = None;
            while !flag.load(Ordering::Relaxed) {
                let now = std::fs::read_to_string(&file)
                    .map(Arc::from)
                    .map_err(|e| format!("{label}: {e}"));
                if last.as_ref() != Some(&now) {
                    if tx.send(now.clone()).is_err() {
                        return;
                    }
                    last = Some(now);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
    if let Err(e) = spawned {
        let (tx, rx) = channel();
        let _ = tx.send(Err(format!("cannot start file watcher: {e}")));
        return Watcher { path, stop, rx };
    }
    Watcher { path, stop, rx }
}

/// The source of an inline kernel operator: its `code` parameter, or the
/// file its `file` parameter names, read again whenever it is saved
/// (offline: once, as it was when the render started).
#[derive(Debug, Default)]
pub struct KernelSource {
    watcher: Option<Watcher>,
    /// The file's latest text.
    file_text: Option<Text>,
    /// The inline code (kept so cooking allocates nothing).
    code: Option<Arc<str>>,
}

impl KernelSource {
    /// The source to run; `Err` when the file cannot be read. Sets
    /// `cook_again` while it watches a file live.
    pub fn get(&mut self, params: Params<'_>, cook_again: &mut bool) -> Text {
        let file = params.str("file");
        if file.is_empty() {
            self.watcher = None;
            self.file_text = None;
            let code = params.str("code");
            if self.code.as_deref() != Some(code) {
                self.code = Some(Arc::from(code));
            }
            return Ok(self.code.clone().expect("set above"));
        }
        if self.watcher.as_ref().is_none_or(|w| w.path != file) {
            self.watcher = Some(watch(file.to_owned(), params.resolve(file)));
            self.file_text = None;
        }
        let w = self.watcher.as_ref().expect("set above");
        let mode = params.mode();
        if mode.is_offline() {
            if self.file_text.is_none() {
                self.file_text = Some(
                    mode.receive(&w.rx)
                        .unwrap_or_else(|_| Err("kernel file watcher stopped".into())),
                );
            }
        } else {
            while let Ok(text) = w.rx.try_recv() {
                self.file_text = Some(text);
            }
            // Keep looking for saves.
            *cook_again = true;
        }
        match &self.file_text {
            Some(text) => text.clone(),
            None => Err("reading the kernel file".into()),
        }
    }
}

/// The text a project kernel node runs: the shared file text, refreshed
/// when the project read a new version.
#[derive(Debug)]
pub struct ProjectSource {
    shared: Arc<SharedSource>,
    seen: u64,
    text: Text,
}

impl ProjectSource {
    pub fn new(shared: Arc<SharedSource>) -> Self {
        Self {
            shared,
            seen: 0,
            text: Err("reading the kernel file".into()),
        }
    }

    /// The latest text (an `Arc` clone).
    pub fn get(&mut self) -> Text {
        if let Some(text) = self.shared.newer(&mut self.seen) {
            self.text = text;
        }
        self.text.clone()
    }
}

/// Renders compile diagnostics with their lines and columns.
pub fn render_errors(ds: &[atd_kernel::Diagnostic], source: &str) -> String {
    ds.iter()
        .map(|d| d.render("kernel", source))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `img.kernel` schema.
pub fn schema() -> OperatorSchema {
    let mut s = pixels_schema(
        "img.kernel",
        "Kernel",
        "A pixel kernel in the Kernel DSL (see the Kernel DSL reference): runs once for every output pixel. `@Cd` starts as input 0 at this pixel and its final value is the pixel; `@P` is the pixel's uv (0..1 from the top-left), `@ix`/`@iy` its column and row, `@res` the output size; `texture(n, uv)` samples input `n` (0 to 3). `ch(\"name\", default)` parameters take their defaults here; a kernel file in the project's `kernels/` folder becomes an operator with controls for them. The last kernel that compiled keeps running while you edit; errors name the line and column.",
        vec![
            ParamSchema::string("code", "Code", DEFAULT)
                .help("The kernel source, used when File is empty.")
                .multiline(),
            ParamSchema::path("file", "File")
                .help("Path to a `.atk` file holding the kernel; reloads when it is saved."),
        ],
    );
    s.cook_budget_us = 100;
    s
}

/// An IMG schema with the Pixels kernel's inputs (`in0`…`in3`) and output.
pub fn pixels_schema(
    type_id: &str,
    label: &str,
    summary: &str,
    params: Vec<ParamSchema>,
) -> OperatorSchema {
    let socket = |name: String| SocketSchema {
        name,
        wire: WireType::Img,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: type_id.into(),
        family: OpFamily::Img,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: (0..INPUTS).map(|i| socket(format!("in{i}"))).collect(),
        outputs: vec![socket("out".into())],
        capabilities: vec![],
        cook_budget_us: 100,
        // `@Time` is common; the cost is one draw.
        time_dependent: true,
        sink: false,
    }
}

/// A compiled Pixels kernel with its pass.
#[derive(Debug)]
struct Program {
    compiled: Compiled,
    pass: FullscreenPass,
    ubytes: Vec<u8>,
    /// `ch()` values by slot.
    values: Vec<[f32; 4]>,
    /// The source this was compiled from.
    source: Arc<str>,
}

type Built = Result<Program, String>;

/// Compiles `source` for pixels and builds its pass (on a worker thread).
fn build(device: &wgpu::Device, format: wgpu::TextureFormat, source: Arc<str>) -> Built {
    let opts = CompileOptions::new(Target::Pixels);
    let compiled = atd_kernel::compile(&source, &opts).map_err(|ds| render_errors(&ds, &source))?;
    let b = &compiled.bindings;
    let fits = b.uniforms.group == 0
        && b.uniforms.binding == 0
        && b.sampler
            .as_ref()
            .is_none_or(|s| s.group == 0 && s.binding == 1)
        && b.images
            .iter()
            .all(|i| i.group == 1 && i.binding == i.input && (i.input as usize) < INPUTS);
    let vertex = b.vertex_entry_point.as_deref();
    let (true, Some(vertex)) = (fits, vertex) else {
        return Err("internal: unexpected kernel binding layout".into());
    };
    let layout = ModuleLayout {
        vertex_entry: vertex,
        fragment_entry: &b.entry_point,
        uniform_size: b.uniform_size,
        inputs: INPUTS,
    };
    let pass = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        FullscreenPass::try_from_module(device, "img.kernel", format, &compiled.wgsl, &layout)
    }))
    .unwrap_or_else(|_| Err("pipeline creation panicked".into()))
    .map_err(|e| format!("the GPU rejected the kernel: {e}"))?;
    Ok(Program {
        ubytes: vec![0; b.uniform_size as usize],
        values: defaults(&compiled),
        pass,
        source,
        compiled,
    })
}

/// Runs Pixels kernels: compiles each new source on a worker thread and
/// draws the last one that compiled.
#[derive(Debug)]
pub struct PixelsRunner {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    /// The last kernel that compiled.
    program: Option<Program>,
    error: Option<String>,
    /// The source of the compile last started, and its result channel.
    requested: Option<Arc<str>>,
    building: Option<Receiver<Built>>,
    /// Time of the last draw, for `@TimeInc` live.
    last_time: Option<f64>,
}

impl PixelsRunner {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            device: device.clone(),
            format,
            program: None,
            error: None,
            requested: None,
            building: None,
            last_time: None,
        }
    }

    /// Whether a compile is running, for [`GpuOp::status`].
    pub fn status(&self) -> Option<&str> {
        match (&self.building, &self.program) {
            (Some(_), _) => Some("compiling"),
            (None, Some(_)) if self.error.is_some() => {
                Some("running the last kernel that compiled")
            }
            _ => None,
        }
    }

    /// Draws `source` (compiling it first when it is new) with `values`.
    pub fn cook(
        &mut self,
        ctx: &mut CookCtx<'_>,
        source: Text,
        values: ChValues<'_>,
    ) -> Result<(), String> {
        // A file that cannot be read (an editor's save in progress, say)
        // leaves the last kernel running.
        let mut file_error = None;
        let source = match source {
            Ok(s) => Some(s),
            Err(e) => {
                file_error = Some(e);
                self.program.as_ref().map(|p| p.source.clone())
            }
        };
        if let Some(source) = source {
            let wanted = self.program.as_ref().is_none_or(|p| p.source != source);
            if !wanted && self.building.is_none() {
                // Back to the kernel that runs (after a failed edit): forget
                // the failed compile, so the same edit again fails again.
                self.error = None;
                self.requested = None;
            }
            if wanted && self.requested.as_ref() != Some(&source) {
                let (tx, rx) = channel();
                let (device, format, text) = (self.device.clone(), self.format, source.clone());
                let spawned = std::thread::Builder::new()
                    .name("atd-kernel-compile".into())
                    .spawn(move || {
                        let _ = tx.send(build(&device, format, text));
                    });
                self.requested = Some(source);
                match spawned {
                    Ok(_) => self.building = Some(rx),
                    Err(e) => self.error = Some(format!("cannot start the kernel compiler: {e}")),
                }
            }
        }
        if let Some(rx) = &self.building {
            match ctx.params.mode().receive(rx) {
                Ok(Ok(program)) => {
                    self.building = None;
                    self.program = Some(program);
                    self.error = None;
                }
                Ok(Err(e)) => {
                    self.building = None;
                    self.error = Some(e);
                }
                Err(TryRecvError::Empty) => ctx.cook_again = true,
                Err(TryRecvError::Disconnected) => {
                    self.building = None;
                    self.error = Some("the kernel compiler stopped".into());
                }
            }
        }
        match self.program.as_mut() {
            Some(p) => {
                fill_values(&p.compiled, values, &mut p.values);
                // Offline the step is fixed; live it is the time since the
                // last draw.
                let dt = ctx
                    .params
                    .mode()
                    .step()
                    .unwrap_or_else(|| self.last_time.map_or(0.0, |t| (ctx.time - t).max(0.0)));
                self.last_time = Some(ctx.time);
                let globals = FrameGlobals {
                    time: ctx.time as f32,
                    time_inc: dt as f32,
                    frame: ctx.frame as f32,
                    width: ctx.output.width(),
                    height: ctx.output.height(),
                };
                p.compiled
                    .write_uniforms(&globals, &p.values, &mut p.ubytes);
                let (inputs, black) = (ctx.inputs, ctx.black);
                let ins: [&wgpu::Texture; INPUTS] = std::array::from_fn(|i| {
                    inputs.get(i).and_then(Option::as_ref).unwrap_or(black)
                });
                p.pass.draw_bytes(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    &ins,
                    &p.ubytes,
                    ctx.output,
                );
            }
            None => clear(ctx.encoder, ctx.output),
        }
        match file_error.as_ref().or(self.error.as_ref()) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

/// `img.kernel`: an inline or file Pixels kernel.
struct InlineKernel {
    source: KernelSource,
    runner: PixelsRunner,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(InlineKernel {
        source: KernelSource::default(),
        runner: PixelsRunner::new(device, format),
    })
}

impl GpuOp for InlineKernel {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let text = self.source.get(ctx.params, &mut ctx.cook_again);
        self.runner.cook(ctx, text, ChValues::Defaults)
    }

    fn status(&self) -> Option<&str> {
        self.runner.status()
    }
}

/// A project's `kernels/<name>.pixels.atk` operator: the file's kernel
/// with the node's parameters as its `ch()` values.
struct ProjectKernel {
    source: ProjectSource,
    runner: PixelsRunner,
}

/// A node of a project Pixels kernel operator reading `shared`.
pub fn create_project(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    shared: Arc<SharedSource>,
) -> Box<dyn GpuOp> {
    Box::new(ProjectKernel {
        source: ProjectSource::new(shared),
        runner: PixelsRunner::new(device, format),
    })
}

impl GpuOp for ProjectKernel {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let text = self.source.get();
        let params = ctx.params;
        self.runner.cook(ctx, text, ChValues::Params(params))
    }

    fn status(&self) -> Option<&str> {
        self.runner.status()
    }
}
