//! A project's ComfyUI workflows as operators (FR-A10, D9,
//! `docs/comfy.md`).
//!
//! Every `comfy/<name>.json` (an API-format workflow) with its
//! `comfy/<name>.info.json` (the server's `/object_info` for the classes it
//! uses, cached at import so the project opens offline and the same files
//! always give the same operator) is an operator type `comfy.<name>` with
//! [`COMFY_CUSTOM`] provenance. Its parameters follow
//! [`atd_ai::comfy::derive`]; each `LoadImage` node is an IMG input and the
//! first `SaveImage`/`PreviewImage` is the IMG output.
//!
//! A node runs its workflow on a ComfyUI server the user runs (never one
//! Atidarla bundles or starts: ComfyUI is GPL-3.0, CLAUDE.md rule 3). Live,
//! everything that waits happens on the node's worker thread: the cook
//! records a copy of each input image, hands the pixels over once the GPU
//! has them (a frame or two later), and uploads a result when one has
//! arrived; it never waits (rule 1), and on frames where nothing arrives it
//! only draws the image it holds (rule 2). At most one prompt is in flight
//! per node. Offline (`atidarla render`) a frame waits for its run.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

pub use atd_ai::comfy::DEFAULT_SERVER;
use atd_ai::comfy::{self as api, Client, Derived, Job, Param, Upload, Widget, Workflow};
use atd_core::registry::{
    COMFY_CUSTOM, OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType,
};
use atd_gpu::capture::Capture;
use atd_gpu::engine::{CookCtx, GpuOp, OpLibrary, Params};
use atd_gpu::fullscreen::FullscreenPass;
use atd_io::image::Image;
use serde_json::{Value, json};

/// The folder of a project that holds its workflows.
pub const COMFY_DIR: &str = "comfy";

/// Prefix of the type IDs of project workflow operators.
pub const TYPE_PREFIX: &str = "comfy.";

/// Suffix of the cached object_info next to a workflow.
pub const INFO_SUFFIX: &str = ".info.json";

/// Parameters every workflow operator has; workflow parameters with these
/// names get their node ID appended.
pub const RESERVED: [&str; 3] = ["server", "run", "auto_run"];

/// With Auto Run, how long parameters and inputs must stay still before a
/// run starts...
const DEBOUNCE: Duration = Duration::from_millis(250);
/// ...unless they have been changing for this long (an animated input still
/// runs, as often as the server keeps up).
const MAX_WAIT: Duration = Duration::from_secs(1);

/// A problem with a file in `comfy/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowError {
    /// Relative to the project: `comfy/<file>`.
    pub file: String,
    pub message: String,
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.file, self.message)
    }
}

impl std::error::Error for WorkflowError {}

/// A workflow operator: its files, read and derived.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    /// `invert` for `comfy/invert.json`.
    pub name: String,
    pub workflow: Workflow,
    pub derived: Derived,
}

impl Definition {
    pub fn type_id(&self) -> String {
        format!("{TYPE_PREFIX}{}", self.name)
    }
}

/// Lower-case ASCII letters, digits and `_`, starting with a letter or `_`.
fn valid_name(name: &str) -> bool {
    let mut b = name.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("not JSON: {e}"))
}

/// Reads `comfy/<name>.json` and its `.info.json` in `project`.
pub fn load_definition(project: &Path, name: &str) -> Result<Definition, WorkflowError> {
    let dir = project.join(COMFY_DIR);
    let err = |file: String, message: String| WorkflowError {
        file: format!("{COMFY_DIR}/{file}"),
        message,
    };
    let file = format!("{name}.json");
    if !valid_name(name) {
        return Err(err(
            file,
            "workflow file names are lower-case letters, digits and _ (they name the operator `comfy.<name>`)"
                .into(),
        ));
    }
    let wf = read_json(&dir.join(&file))
        .and_then(|v| api::parse_workflow(&v).map_err(|e| e.to_string()))
        .map_err(|m| err(file.clone(), m))?;
    let info_file = format!("{name}{INFO_SUFFIX}");
    let info = read_json(&dir.join(&info_file))
        .and_then(|v| api::parse_object_info(&v).map_err(|e| e.to_string()))
        .map_err(|m| {
            err(
                info_file.clone(),
                format!("{m} (import the workflow again with `atidarla comfy import` to write it)"),
            )
        })?;
    let derived = api::derive(&wf, &info, &RESERVED);
    if let Some(class) = derived.unknown_classes.first() {
        return Err(err(
            info_file,
            format!(
                "has no entry for node class `{class}` (import the workflow again with the server that runs it)"
            ),
        ));
    }
    let def = Definition {
        name: name.to_owned(),
        workflow: wf,
        derived,
    };
    // Every parameter must accept its own default.
    for p in &schema(&def).params {
        p.validate(&p.default)
            .map_err(|e| err(file.clone(), format!("parameter `{}`: {e}", p.name)))?;
    }
    Ok(def)
}

/// The parameter for one workflow input.
fn param_schema(p: &Param) -> ParamSchema {
    let s = match &p.widget {
        Widget::Int {
            default, min, max, ..
        } => ParamSchema::int(&p.id, &p.label, *default, *min, *max),
        Widget::Float { default, min, max } => {
            ParamSchema::float(&p.id, &p.label, *default, *min, *max)
        }
        Widget::String { default, multiline } => {
            let s = ParamSchema::string(&p.id, &p.label, default);
            if *multiline { s.multiline() } else { s }
        }
        Widget::Boolean { default } => ParamSchema::toggle(&p.id, &p.label, *default),
        Widget::Combo { options, default } => {
            let options: Vec<&str> = options.iter().map(String::as_str).collect();
            ParamSchema::menu(&p.id, &p.label, &options, default.as_deref().unwrap_or(""))
        }
    };
    s.help(&p.help)
}

/// The operator a workflow declares.
pub fn schema(def: &Definition) -> OperatorSchema {
    let d = &def.derived;
    let mut params = vec![
        ParamSchema::string("server", "Server", DEFAULT_SERVER).help(
            "The ComfyUI server, `host:port`: this machine or the LAN (Sovereign mode refuses internet servers). Atidarla does not start ComfyUI; run it yourself.",
        ),
        ParamSchema::pulse("run", "Run").help("Runs the workflow now with the current parameters and inputs."),
        ParamSchema::toggle("auto_run", "Auto Run", false).help(
            "Runs again whenever a parameter or input changes (after they settle for a moment; one run at a time).",
        ),
    ];
    for p in &d.params {
        params.push(param_schema(p));
        if let Some(r) = &p.random {
            params.push(
                ParamSchema::toggle(r, &format!("{} randomise", p.label), false).help(&format!(
                    "Each run uses a new random `{}` (the parameter keeps its value).",
                    p.input
                )),
            );
        }
    }
    let inputs = d
        .images
        .iter()
        .map(|i| SocketSchema {
            name: i.socket.clone(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        })
        .collect();
    let images = if d.images.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = d
            .images
            .iter()
            .map(|i| format!("`{}` feeds {}", i.socket, i.label))
            .collect();
        format!(" Inputs: {}.", names.join(", "))
    };
    let output = match &d.output {
        Some(n) => format!(
            " The output is node {n}'s first image ({}).",
            def.workflow.node(n).map_or("?", |n| n.class_type.as_str())
        ),
        None => " It has no SaveImage or PreviewImage node, so its output stays empty.".into(),
    };
    let label = {
        let words = def.name.replace('_', " ");
        let mut c = words.trim().chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect())
            .unwrap_or_default()
    };
    OperatorSchema {
        type_id: def.type_id(),
        family: OpFamily::Ai,
        label,
        summary: format!(
            "The project's ComfyUI workflow `{COMFY_DIR}/{}.json`, run on a ComfyUI server.{images}{output}",
            def.name
        ),
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
        // The cook copies an input, uploads a result when one arrives and
        // draws; the work happens on the server.
        cook_budget_us: 250,
        time_dependent: false,
        sink: false,
    }
}

/// A project's workflow operators.
#[derive(Debug, Default)]
pub struct ProjectComfy {
    /// By operator name.
    ops: BTreeMap<String, (OperatorSchema, Arc<Definition>)>,
    errors: Vec<WorkflowError>,
}

impl ProjectComfy {
    /// Reads the project's `comfy/` files (a missing folder means none).
    pub fn open(project: &Path) -> Self {
        let mut out = Self::default();
        let Ok(list) = std::fs::read_dir(project.join(COMFY_DIR)) else {
            return out;
        };
        let mut names: Vec<String> = list
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|f| !f.ends_with(INFO_SUFFIX))
            .filter_map(|f| f.strip_suffix(".json").map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            match load_definition(project, &name) {
                Ok(def) => {
                    out.ops.insert(name, (schema(&def), Arc::new(def)));
                }
                Err(e) => out.errors.push(e),
            }
        }
        out
    }

    /// No workflow files (or none that loaded, and no errors).
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.errors.is_empty()
    }

    pub fn schemas(&self) -> impl Iterator<Item = &OperatorSchema> {
        self.ops.values().map(|(s, _)| s)
    }

    pub fn errors(&self) -> &[WorkflowError] {
        &self.errors
    }

    /// Why `type_id` (a `comfy.` operator a network uses) is not registered.
    pub fn explain(&self, type_id: &str) -> Option<String> {
        let name = type_id.strip_prefix(TYPE_PREFIX)?;
        let file = format!("{COMFY_DIR}/{name}.json");
        Some(
            match self
                .errors
                .iter()
                .find(|e| e.file == file || e.file == format!("{COMFY_DIR}/{name}{INFO_SUFFIX}"))
            {
                Some(e) => e.to_string(),
                None => format!("{file} does not exist"),
            },
        )
    }

    /// Registers every operator, replacing those of a project opened
    /// before. Call before loading the network.
    pub fn register(&self, registry: &mut Registry, library: &mut OpLibrary) {
        clear_custom(registry);
        for (schema, def) in self.ops.values() {
            if let Err(e) = registry.replace_custom(schema.clone(), COMFY_CUSTOM) {
                log::warn!("{COMFY_DIR}/{}.json: {e}", def.name);
                continue;
            }
            let def = def.clone();
            library.add(schema.type_id.clone(), move |device, format| {
                Box::new(ComfyOp::new(device, format, def.clone()))
            });
        }
    }
}

/// Removes every operator registered from workflow files (another project
/// is opening). Load a network afterwards.
pub fn clear_custom(registry: &mut Registry) {
    let ids: Vec<String> = registry
        .iter()
        .filter(|s| registry.provenance(&s.type_id) == Some(COMFY_CUSTOM))
        .map(|s| s.type_id.clone())
        .collect();
    for id in ids {
        registry.remove_custom(&id);
    }
}

// ------------------------------------------------------------------- import

/// What [`import`] wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub type_id: String,
    /// Relative to the project.
    pub files: [String; 2],
    pub params: usize,
    pub images: usize,
    pub has_output: bool,
}

/// Imports an API-format workflow into `project`: fetches the server's
/// object_info and writes `comfy/<name>.json` and `comfy/<name>.info.json`.
/// `name` defaults to the workflow file's name. Blocks on the network: not
/// for the render thread.
pub fn import(
    workflow: &Path,
    project: &Path,
    server: &str,
    name: Option<&str>,
) -> Result<Imported, String> {
    if !project.join("project.toml").is_file() {
        return Err(format!(
            "{} is not a project folder (no project.toml)",
            project.display()
        ));
    }
    let v = read_json(workflow).map_err(|e| format!("{}: {e}", workflow.display()))?;
    let wf = api::parse_workflow(&v).map_err(|e| format!("{}: {e}", workflow.display()))?;
    let name = match name {
        Some(n) => n.to_owned(),
        None => api::ident(
            &workflow
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
    };
    if !valid_name(&name) {
        return Err(format!(
            "`{name}`: operator names are lower-case letters, digits and _ (pass --name)"
        ));
    }
    let client = Client::new(server).map_err(|e| e.to_string())?;
    let info = client.object_info().map_err(|e| e.to_string())?;
    let missing: Vec<&str> = wf
        .classes()
        .into_iter()
        .filter(|c| info.get(*c).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{server} does not have node class{} {} (install the custom nodes the workflow needs, then import again)",
            if missing.len() == 1 { "" } else { "es" },
            missing.join(", ")
        ));
    }
    let dir = project.join(COMFY_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let files = [format!("{name}.json"), format!("{name}{INFO_SUFFIX}")];
    for (file, value) in files
        .iter()
        .zip([wf.to_value(), api::info_subset(&wf, &info)])
    {
        let text = serde_json::to_string_pretty(&value).expect("JSON values serialise") + "\n";
        let path = dir.join(file);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let def = load_definition(project, &name).map_err(|e| e.to_string())?;
    Ok(Imported {
        type_id: def.type_id(),
        files: files.map(|f| format!("{COMFY_DIR}/{f}")),
        params: def.derived.params.len(),
        images: def.derived.images.len(),
        has_output: def.derived.output.is_some(),
    })
}

// ----------------------------------------------------------------- running

/// One run, as the worker gets it.
struct RunJob {
    server: String,
    prompt: Value,
    /// (`LoadImage` node, input pixels) for connected inputs.
    images: Vec<(String, Image)>,
    output: Option<String>,
}

enum Event {
    Progress(String),
    Done { image: Option<Image>, seconds: f64 },
    Failed(String),
}

/// FNV-1a: a stable name for an uploaded frame.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn encode_png(img: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&img.rgba))
        .map_err(|e| format!("encoding an input image: {e}"))?;
    Ok(out)
}

/// Runs `job` to the end (blocking): PNG-encodes and uploads the inputs,
/// queues the prompt, follows it, decodes the result.
fn execute(
    job: RunJob,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(String),
) -> Result<(Option<Image>, f64), String> {
    let client = Client::new(&job.server).map_err(|e| e.to_string())?;
    let mut uploads = Vec::with_capacity(job.images.len());
    for (node, img) in &job.images {
        let bytes = encode_png(img)?;
        uploads.push(Upload {
            node: node.clone(),
            filename: format!("atd_{}_{:016x}.png", api::ident(node), fnv(&bytes)),
            bytes,
        });
    }
    let outcome = client
        .run(
            Job {
                prompt: job.prompt,
                uploads,
                output: job.output,
            },
            cancel,
            &mut |p| progress(p.to_string()),
        )
        .map_err(|e| e.to_string())?;
    let image = match outcome.image {
        Some(bytes) => {
            Some(atd_io::image::decode(&bytes).map_err(|e| format!("the result image: {e}"))?)
        }
        None => None,
    };
    Ok((image, outcome.seconds))
}

/// The node's worker thread: one run at a time.
struct Worker {
    jobs: Sender<RunJob>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    fn start() -> Result<Self, String> {
        let (jobs, job_rx) = channel::<RunJob>();
        let (tx, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::Builder::new()
            .name("atd-comfy".into())
            .spawn(move || {
                for job in job_rx {
                    let mut progress = |s: String| {
                        let _ = tx.send(Event::Progress(s));
                    };
                    let event = match execute(job, &flag, &mut progress) {
                        Ok((image, seconds)) => Event::Done { image, seconds },
                        Err(e) => Event::Failed(e),
                    };
                    if tx.send(event).is_err() || flag.load(Ordering::Acquire) {
                        return;
                    }
                }
            })
            .map_err(|e| format!("cannot start the ComfyUI worker: {e}"))?;
        Ok(Self {
            jobs,
            events,
            cancel,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The thread ends after its run notices; the server may finish the
        // prompt on its own.
        self.cancel.store(true, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Input images are on their way back from the GPU.
    Reading,
    /// A prompt is with the worker.
    Running,
}

/// An input image being read back for a run.
struct Readback {
    capture: Capture,
    pixels: Vec<u8>,
    /// A copy is in flight.
    pending: bool,
}

const SHOW: &str = "return textureSample(in0, samp, uv);";

/// A node of a workflow operator.
struct ComfyOp {
    def: Arc<Definition>,
    pass: FullscreenPass,
    /// The last result.
    held: Option<wgpu::Texture>,
    worker: Option<Worker>,
    status: String,
    phase: Phase,
    /// `run` as last seen (a change is a press).
    last_run: Option<bool>,
    last_auto: bool,
    /// Hash of what the last run would send, as last seen.
    key: Option<u64>,
    /// A run has started (Auto Run's first run, and offline: a result
    /// exists for `key`).
    ran: bool,
    /// A run is wanted: since when, and the latest change.
    want: Option<(Instant, Instant)>,
    /// The wanted run was asked for with Run (no waiting).
    pressed: bool,
    reads: Vec<Option<Readback>>,
    runs: u64,
}

impl std::fmt::Debug for ComfyOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComfyOp")
            .field("type", &self.def.type_id())
            .field("phase", &self.phase)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl ComfyOp {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, def: Arc<Definition>) -> Self {
        let n = def.derived.images.len();
        Self {
            pass: FullscreenPass::new(device, &def.type_id(), format, 1, "", SHOW),
            def,
            held: None,
            worker: None,
            status: "not run yet: press Run, or turn on Auto Run".into(),
            phase: Phase::Idle,
            last_run: None,
            last_auto: false,
            key: None,
            ran: false,
            want: None,
            pressed: false,
            reads: (0..n).map(|_| None).collect(),
            runs: 0,
        }
    }

    /// Hash of every value a run sends (no allocation).
    fn key(&self, p: &Params<'_>) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        p.str("server").hash(&mut h);
        for param in &self.def.derived.params {
            match &param.widget {
                Widget::Int { .. } => int(p, &param.id).hash(&mut h),
                Widget::Float { .. } => float(p, &param.id).to_bits().hash(&mut h),
                Widget::Boolean { .. } => p.bool(&param.id).hash(&mut h),
                Widget::String { .. } | Widget::Combo { .. } => p.str(&param.id).hash(&mut h),
            }
            if let Some(r) = &param.random {
                p.bool(r).hash(&mut h);
            }
        }
        h.finish()
    }

    /// The run's job. `frame` makes random seeds a function of the frame
    /// (offline); live they are fresh.
    fn job(&mut self, p: &Params<'_>, images: Vec<(String, Image)>, frame: Option<u64>) -> RunJob {
        self.runs += 1;
        let mut values = Vec::with_capacity(self.def.derived.params.len());
        for (i, param) in self.def.derived.params.iter().enumerate() {
            let v = match &param.widget {
                Widget::Int { min, max, .. } => {
                    let random = param.random.as_deref().is_some_and(|r| p.bool(r));
                    let n = if random {
                        let salt = match frame {
                            Some(f) => f.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ i as u64,
                            None => {
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_nanos() as u64)
                                    ^ self.runs.rotate_left(32)
                            }
                        };
                        random_in(salt, *min, *max)
                    } else {
                        int(p, &param.id)
                    };
                    json!(n)
                }
                Widget::Float { .. } => json!(float(p, &param.id)),
                Widget::Boolean { .. } => json!(p.bool(&param.id)),
                Widget::String { .. } | Widget::Combo { .. } => json!(p.str(&param.id)),
            };
            values.push((param.node.clone(), param.input.clone(), v));
        }
        RunJob {
            server: p.str("server").to_owned(),
            prompt: api::build_prompt(&self.def.workflow, &values, &[]),
            images,
            output: self.def.derived.output.clone(),
        }
    }

    /// Keeps `img` as the result.
    fn take_result(&mut self, ctx: &CookCtx<'_>, img: &Image) {
        let fits = self
            .held
            .as_ref()
            .is_some_and(|t| t.width() == img.width && t.height() == img.height);
        if !fits {
            self.held = Some(ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("atd-comfy-result"),
                size: wgpu::Extent3d {
                    width: img.width,
                    height: img.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
        }
        let held = self.held.as_ref().expect("made above");
        atd_interop::readback::write_rgba8(ctx.queue, held, &img.rgba);
    }

    /// Draws the result held, or nothing.
    fn draw(&self, ctx: &mut CookCtx<'_>) {
        let src = self.held.as_ref().unwrap_or(ctx.black);
        self.pass
            .draw(ctx.device, ctx.queue, ctx.encoder, &[src], &[], ctx.output);
    }

    /// Takes what the worker sent. Returns whether a result of another
    /// size arrived (the output takes its size next frame).
    fn collect(&mut self, ctx: &CookCtx<'_>) -> bool {
        let mut resized = false;
        while let Some(worker) = &self.worker {
            match worker.events.try_recv() {
                Ok(Event::Progress(s)) => self.status = s,
                Ok(Event::Done { image, seconds }) => {
                    self.phase = Phase::Idle;
                    self.status = format!("done in {seconds:.1} s");
                    if let Some(img) = image {
                        let before = self.held.as_ref().map(|t| (t.width(), t.height()));
                        self.take_result(ctx, &img);
                        resized |= before != Some((img.width, img.height));
                    }
                }
                Ok(Event::Failed(e)) => {
                    self.phase = Phase::Idle;
                    self.status = e;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.phase = Phase::Idle;
                    self.status = "the ComfyUI worker stopped".into();
                }
            }
        }
        resized
    }

    fn send(&mut self, job: RunJob) {
        if self.worker.is_none() {
            match Worker::start() {
                Ok(w) => self.worker = Some(w),
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        }
        let worker = self.worker.as_ref().expect("started above");
        if worker.jobs.send(job).is_ok() {
            self.phase = Phase::Running;
            self.status = "connecting".into();
        } else {
            self.worker = None;
            self.status = "the ComfyUI worker stopped".into();
        }
    }

    /// Starts a run: reads back the connected inputs first, if any.
    fn start(&mut self, ctx: &mut CookCtx<'_>) {
        self.want = None;
        self.pressed = false;
        self.ran = true;
        let mut reading = false;
        for (i, slot) in self.reads.iter_mut().enumerate() {
            let Some(tex) = ctx.inputs.get(i).and_then(Option::as_ref) else {
                *slot = None;
                continue;
            };
            let (w, h) = (tex.width(), tex.height());
            if slot.as_ref().is_none_or(|r| r.capture.size() != (w, h)) {
                *slot = Some(Readback {
                    capture: Capture::new(ctx.device, w, h, 1),
                    pixels: vec![0; (w * h * 4) as usize],
                    pending: false,
                });
            }
            let r = slot.as_mut().expect("set above");
            r.pending = r.capture.record(ctx.encoder, tex);
            reading |= r.pending;
        }
        if reading {
            self.phase = Phase::Reading;
            self.status = "reading the input images".into();
        } else {
            let job = self.job(&ctx.params, Vec::new(), None);
            self.send(job);
        }
    }

    /// Collects finished input copies; once all are in, sends the run.
    fn read(&mut self, ctx: &CookCtx<'_>) {
        for r in self.reads.iter_mut().flatten().filter(|r| r.pending) {
            let w = r.capture.size().0 as usize * 4;
            let pixels = &mut r.pixels;
            let mut last = false;
            let h = r.capture.size().1;
            r.capture.poll(ctx.device, |row, y| {
                pixels[y as usize * w..(y as usize + 1) * w].copy_from_slice(row);
                last |= y + 1 == h;
            });
            if last {
                r.pending = false;
            }
        }
        if self.reads.iter().flatten().any(|r| r.pending) {
            return;
        }
        let mut images = Vec::new();
        for (r, input) in self.reads.iter_mut().zip(&self.def.derived.images) {
            if let Some(r) = r {
                let (width, height) = r.capture.size();
                images.push((
                    input.node.clone(),
                    Image {
                        width,
                        height,
                        rgba: r.pixels.clone(),
                    },
                ));
            }
        }
        let job = self.job(&ctx.params, images, None);
        self.send(job);
    }

    fn cook_live(&mut self, ctx: &mut CookCtx<'_>) {
        let p = ctx.params;
        let now = Instant::now();
        let resized = self.collect(ctx);
        let run = p.bool("run");
        if self.last_run.is_some_and(|r| r != run) {
            self.pressed = true;
            self.want = Some((now, now));
        }
        self.last_run = Some(run);
        let key = self.key(&p);
        let changed = self.key != Some(key) || ctx.inputs_cooked;
        self.key = Some(key);
        let auto = p.bool("auto_run");
        if auto && (changed || !self.ran || !self.last_auto) && !self.pressed {
            self.want = Some(match self.want {
                Some((first, _)) => (first, now),
                None => (now, now),
            });
        }
        self.last_auto = auto;
        if self.phase == Phase::Reading {
            self.read(ctx);
        }
        if self.phase == Phase::Idle
            && let Some((first, last)) = self.want
            && (self.pressed || now - last >= DEBOUNCE || now - first >= MAX_WAIT)
        {
            self.start(ctx);
        }
        ctx.cook_again = resized || self.phase != Phase::Idle || self.want.is_some();
    }

    fn cook_offline(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let key = self.key(&p);
        let needed = !self.ran || self.key != Some(key) || ctx.inputs_cooked;
        self.key = Some(key);
        if !needed {
            return Ok(());
        }
        self.ran = true;
        let mut images = Vec::new();
        if ctx.inputs.iter().any(Option::is_some) {
            // The inputs were drawn earlier in this frame's encoder: submit
            // it so they can be read back now.
            let done = std::mem::replace(
                ctx.encoder,
                ctx.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("atd-comfy-offline"),
                    }),
            );
            ctx.queue.submit([done.finish()]);
            for (input, tex) in self.def.derived.images.iter().zip(ctx.inputs) {
                let Some(tex) = tex else { continue };
                let rgba = atd_interop::readback::read_rgba8(ctx.device, ctx.queue, tex)
                    .map_err(|e| format!("reading input {}: {e}", input.socket))?;
                images.push((
                    input.node.clone(),
                    Image {
                        width: tex.width(),
                        height: tex.height(),
                        rgba,
                    },
                ));
            }
        }
        let job = self.job(&p, images, Some(ctx.frame));
        let (image, seconds) = execute(job, &AtomicBool::new(false), &mut |_| {})?;
        self.status = format!("done in {seconds:.1} s");
        if let Some(img) = image {
            self.take_result(ctx, &img);
        }
        Ok(())
    }
}

impl GpuOp for ComfyOp {
    fn output_size(
        &self,
        _params: &Params<'_>,
        _input: Option<(u32, u32)>,
        _project: (u32, u32),
    ) -> Option<(u32, u32)> {
        self.held.as_ref().map(|t| (t.width(), t.height()))
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        if ctx.params.mode().is_offline() {
            self.cook_offline(ctx)?;
        } else {
            // Problems (no server, a refused workflow) show in the status
            // line; the node keeps its last result.
            self.cook_live(ctx);
        }
        self.draw(ctx);
        Ok(())
    }

    fn after_submit(&mut self) {
        for r in self.reads.iter_mut().flatten() {
            r.capture.after_submit();
        }
    }

    fn status(&self) -> Option<&str> {
        Some(&self.status)
    }
}

/// An integer parameter as the run sends it.
fn int(p: &Params<'_>, name: &str) -> i64 {
    p.i64(name)
}

/// A float parameter at full precision unless a live value, binding or
/// expression drives it.
fn float(p: &Params<'_>, name: &str) -> f64 {
    p.live(name)
        .and_then(Value::as_f64)
        .or_else(|| p.bound(name).map(f64::from))
        .or_else(|| p.expr(name))
        .or_else(|| p.value(name).and_then(Value::as_f64))
        .unwrap_or(0.0)
}

/// A pseudo-random integer from `salt` within the limits (at most 2³²
/// values above the minimum, as ComfyUI's own randomise does in practice).
fn random_in(salt: u64, min: Option<i64>, max: Option<i64>) -> i64 {
    let mut z = salt.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    let lo = min.unwrap_or(0);
    let hi = max
        .unwrap_or(i64::MAX)
        .min(lo.saturating_add(u32::MAX as i64));
    let span = (i128::from(hi) - i128::from(lo) + 1).max(1) as u128;
    (i128::from(lo) + (u128::from(z) % span) as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_random_seeds() {
        assert!(valid_name("invert_2") && !valid_name("Invert") && !valid_name("2x"));
        for salt in 0..100 {
            let v = random_in(salt, Some(5), Some(9));
            assert!((5..=9).contains(&v), "{v}");
        }
        assert_eq!(random_in(7, Some(0), None), random_in(7, Some(0), None));
        assert!(random_in(1, Some(0), Some(i64::MAX)) <= u32::MAX as i64);
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
    }
}
