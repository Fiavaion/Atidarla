//! Opening and saving project folders (`*.atidarla/`, PRD §5.5, A11).
//!
//! File dialogs block until the user answers, so they run on their own
//! thread and report back through a channel: the patch keeps cooking while
//! a dialog is open (rule 1).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_core::graph::{Edit, Network, NodeId, Param};
use atd_core::registry::{ParamKind, WireType};
use atd_format::Project;
use atd_gpu::engine::Engine;
use serde_json::Value;

type ModelPins = std::collections::BTreeMap<String, atd_format::ModelPin>;

/// Something that would discard unsaved changes, waiting for confirmation.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    New,
    /// Show the Open dialog.
    Open,
    /// Open this folder.
    OpenDir(PathBuf),
    Quit,
}

#[derive(Debug)]
enum Dialog {
    Open(Receiver<Option<PathBuf>>),
    SaveAs(Receiver<Option<PathBuf>>),
    /// A ComfyUI workflow to import into the open project.
    ComfyWorkflow(Receiver<Option<PathBuf>>),
}

/// A ComfyUI workflow import running on its own thread (it reads the
/// server's object_info).
type ComfyImport = Receiver<Result<atd_ops_ai::comfy::Imported, String>>;

/// The open project and its unsaved-change state.
#[derive(Debug)]
pub struct ProjectState {
    /// Folder of the open project; `None` until an untitled one is saved.
    pub dir: Option<PathBuf>,
    /// Name, description and resolution (networks live in the engine).
    pub meta: Project,
    /// The network as last opened or saved.
    saved: Network,
    /// The mappings as last opened or saved.
    saved_mappings: atd_format::Mappings,
    /// The timeline as last opened or saved.
    saved_timeline: Option<atd_format::TimelineDoc>,
    /// `[ai]` and `[[models]]` as last opened or saved.
    saved_ai: (atd_format::AiSettings, ModelPins),
    dialog: Option<Dialog>,
    /// An action waiting for "discard unsaved changes?".
    pub confirm: Option<Action>,
    /// The last result to show the user: (text, is an error).
    pub message: Option<(String, bool)>,
    /// The open project's Python operators (`ops/*.py`, docs/python-ops.md).
    pub python: Option<atd_py::ops::PythonOps>,
    /// Ask whether to run them (they are not allowed yet).
    pub python_ask: bool,
    /// The open project's kernel operators (`kernels/*.atk`,
    /// docs/kernel-dsl.md).
    pub kernels: Option<atd_ops_pnt::project::ProjectKernels>,
    /// The open project's ComfyUI workflow operators (`comfy/*.json`,
    /// docs/comfy.md).
    pub comfy: Option<atd_ops_ai::comfy::ProjectComfy>,
    comfy_import: Option<ComfyImport>,
    /// Counts projects opened or started; the textport starts afresh when
    /// it changes.
    pub opened: u64,
}

impl ProjectState {
    /// An untitled project holding `net`, counted as saved.
    pub fn untitled(net: &Network) -> Self {
        Self {
            dir: None,
            meta: Project::new("Untitled"),
            saved: net.clone(),
            saved_mappings: Default::default(),
            saved_timeline: None,
            saved_ai: Default::default(),
            dialog: None,
            confirm: None,
            message: None,
            python: None,
            python_ask: false,
            kernels: None,
            comfy: None,
            comfy_import: None,
            opened: 0,
        }
    }

    pub fn dirty(&self, engine: &Engine) -> bool {
        *engine.network() != self.saved
            || self.meta.mappings != self.saved_mappings
            || self.meta.timeline != self.saved_timeline
            || self.meta.ai != self.saved_ai.0
            || self.meta.models != self.saved_ai.1
    }

    /// The window title: project name, a dot when there are unsaved
    /// changes, and "Sovereign" while Sovereign mode is on (FR-A8).
    pub fn title(&self, engine: Option<&Engine>) -> String {
        let dot = if engine.is_some_and(|e| self.dirty(e)) {
            " •"
        } else {
            ""
        };
        let sovereign = if self.meta.ai.sovereign {
            " · Sovereign"
        } else {
            ""
        };
        format!("{}{dot}{sovereign} — Atidarla", self.meta.name)
    }

    /// Runs `action`, or asks first when it would discard unsaved changes.
    /// Returns the node to view after a load.
    pub fn request(&mut self, engine: &mut Engine, action: Action) -> Option<NodeId> {
        if self.dirty(engine) {
            self.confirm = Some(action);
            return None;
        }
        self.run(engine, action)
    }

    /// Runs `action` without asking. Returns the node to view after a load.
    pub fn run(&mut self, engine: &mut Engine, action: Action) -> Option<NodeId> {
        self.confirm = None;
        match action {
            Action::New => {
                engine.set_project_dir(None);
                if let Err(e) = engine.load(Network::default()) {
                    self.message = Some((e.to_string(), true));
                    return None;
                }
                engine.extend(|r, _| {
                    atd_py::ops::clear_custom(r);
                    atd_ops_pnt::project::clear_custom(r);
                    atd_ops_ai::comfy::clear_custom(r);
                });
                let opened = self.opened + 1;
                *self = Self::untitled(engine.network());
                self.opened = opened;
                None
            }
            Action::Open => {
                if self.dialog.is_none() {
                    let pick = rfd::AsyncFileDialog::new()
                        .set_title("Open project (a .atidarla folder)")
                        .pick_folder();
                    self.dialog = Some(Dialog::Open(spawn(pick)));
                }
                None
            }
            Action::OpenDir(dir) => match open(engine, &dir) {
                Ok(Opened {
                    project: meta,
                    view,
                    python,
                    kernels,
                    comfy,
                }) => {
                    self.saved = engine.network().clone();
                    self.message = Some((format!("Opened {}", dir.display()), false));
                    if let Some(e) = comfy.errors().first() {
                        self.message = Some((e.to_string(), true));
                    }
                    if let Some(e) = kernels.errors().first() {
                        self.message = Some((e.to_string(), true));
                    }
                    if let Some(e) = python.errors().first() {
                        self.message = Some((e.to_string(), true));
                    }
                    // Script DATs are project Python too: they ask as well.
                    let scripts = engine
                        .network()
                        .nodes()
                        .any(|n| n.type_id == atd_py::script::TYPE_ID);
                    self.python_ask = (!python.is_empty() && !python.is_allowed()) || scripts;
                    self.python = (!python.is_empty() || scripts).then_some(python);
                    self.saved_mappings = meta.mappings.clone();
                    self.saved_timeline = meta.timeline.clone();
                    self.saved_ai = (meta.ai.clone(), meta.models.clone());
                    self.kernels = (!kernels.is_empty()).then_some(kernels);
                    self.comfy = (!comfy.is_empty()).then_some(comfy);
                    self.meta = meta;
                    self.dir = Some(dir);
                    self.opened += 1;
                    view
                }
                Err(e) => {
                    self.message = Some((e, true));
                    None
                }
            },
            // The caller closes the window.
            Action::Quit => None,
        }
    }

    /// Saves to the project folder, or asks for one first. `store` gives
    /// the edits that store mapped values ([`crate::mappings::Control::stored_values`]).
    pub fn save(&mut self, engine: &mut Engine, store: impl FnOnce(&Engine) -> Vec<Edit>) {
        match self.dir.clone() {
            Some(dir) => self.save_to(engine, &dir, store(engine)),
            None => self.save_as(),
        }
    }

    /// Asks where to save, then saves there.
    pub fn save_as(&mut self) {
        if self.dialog.is_some() {
            return;
        }
        let name = format!("{}.{}", file_stem(&self.meta.name), atd_format::EXTENSION);
        let pick = rfd::AsyncFileDialog::new()
            .set_title("Save project as")
            .set_file_name(name)
            .save_file();
        self.dialog = Some(Dialog::SaveAs(spawn(pick)));
    }

    /// Saves with `store` applied to the saved network; only once the files
    /// are written does the patch take them too (one undo step), so a
    /// failed save edits nothing.
    pub(crate) fn save_to(&mut self, engine: &mut Engine, dir: &Path, store: Vec<Edit>) {
        if !self.meta.packs.is_empty() {
            // The patch shows Pack instances expanded (`open`): saving it
            // would replace them with their insides.
            self.message = Some((PACKS_READ_ONLY.into(), true));
            return;
        }
        match save(engine, &self.meta, dir, &store) {
            Ok(()) => {
                // Mapped parameters keep what the controls set
                // (docs/mappings.md).
                if !store.is_empty()
                    && let Err(e) = engine.edit("Store mapped values", store)
                {
                    self.message = Some((e.to_string(), true));
                    return;
                }
                self.saved = engine.network().clone();
                self.saved_mappings = self.meta.mappings.clone();
                self.saved_timeline = self.meta.timeline.clone();
                self.saved_ai = (self.meta.ai.clone(), self.meta.models.clone());
                self.dir = Some(dir.to_owned());
                self.message = Some((format!("Saved {}", dir.display()), false));
            }
            Err(e) => self.message = Some((e, true)),
        }
    }

    /// Asks for a ComfyUI workflow (API format) and imports it into the
    /// open project as operator `comfy.<name>` (docs/comfy.md). The server
    /// is ComfyUI's default address; `atidarla comfy import --server` takes
    /// another.
    pub fn import_comfy(&mut self) {
        if self.dir.is_none() {
            self.message = Some((
                "Save the project first: an imported workflow is stored in its comfy/ folder"
                    .into(),
                true,
            ));
            return;
        }
        if self.dialog.is_some() || self.comfy_import.is_some() {
            return;
        }
        let pick = rfd::AsyncFileDialog::new()
            .set_title("Import ComfyUI workflow (API format, from Workflow > Export (API))")
            .add_filter("ComfyUI workflow (.json)", &["json"])
            .pick_file();
        self.dialog = Some(Dialog::ComfyWorkflow(spawn(pick)));
    }

    /// Registers the operators of a finished import, so the new one can be
    /// added to the patch.
    fn poll_comfy_import(&mut self, engine: &mut Engine) {
        let Some(rx) = &self.comfy_import else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the import stopped".into()),
        };
        self.comfy_import = None;
        match (result, &self.dir) {
            (Ok(imported), Some(dir)) => {
                let comfy = atd_ops_ai::comfy::ProjectComfy::open(dir);
                engine.extend(|r, l| comfy.register(r, l));
                self.message = Some(match comfy.errors().first() {
                    Some(e) => (e.to_string(), true),
                    None => (
                        format!(
                            "Imported {} ({} parameters, {} image inputs): add it to the patch like any operator",
                            imported.type_id, imported.params, imported.images
                        ),
                        false,
                    ),
                });
                self.comfy = Some(comfy);
            }
            (Ok(_), None) => {}
            (Err(e), _) => self.message = Some((format!("ComfyUI import: {e}"), true)),
        }
    }

    /// Handles a dialog that has been answered. Returns the node to view
    /// after an open.
    /// `store` is as for [`Self::save`].
    pub fn poll(
        &mut self,
        engine: &mut Engine,
        store: impl FnOnce(&Engine) -> Vec<Edit>,
    ) -> Option<NodeId> {
        self.poll_comfy_import(engine);
        let (rx, open) = match &self.dialog {
            Some(Dialog::Open(rx)) => (rx, true),
            Some(Dialog::SaveAs(rx)) => (rx, false),
            Some(Dialog::ComfyWorkflow(rx)) => {
                let picked = match rx.try_recv() {
                    Ok(picked) => picked,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => None,
                };
                self.dialog = None;
                if let (Some(file), Some(dir)) = (picked, self.dir.clone()) {
                    self.start_comfy_import(file, dir);
                }
                return None;
            }
            None => return None,
        };
        let picked = match rx.try_recv() {
            Ok(picked) => picked,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => None,
        };
        self.dialog = None;
        let path = picked?;
        if open {
            self.run(engine, Action::OpenDir(path))
        } else {
            // A project is a folder named `<name>.atidarla`.
            let dir = if path.extension().is_some_and(|e| e == atd_format::EXTENSION) {
                path
            } else {
                path.with_extension(atd_format::EXTENSION)
            };
            if self.dir.is_none() || self.meta.name == "Untitled" {
                self.meta.name = dir
                    .file_stem()
                    .map_or_else(|| "Untitled".into(), |s| s.to_string_lossy().into_owned());
            }
            // Relative paths were relative to the old folder: make them
            // absolute before moving, so they still find their files.
            if engine.project_dir() != Some(dir.as_path()) {
                let edits = absolute_paths(engine);
                if !edits.is_empty()
                    && let Err(e) = engine.edit("Save As: absolute media paths", edits)
                {
                    self.message = Some((e.to_string(), true));
                    return None;
                }
                engine.set_project_dir(Some(dir.clone()));
            }
            let store = store(engine);
            self.save_to(engine, &dir, store);
            None
        }
    }
}

impl ProjectState {
    /// Imports `file` into the project at `dir` on its own thread.
    fn start_comfy_import(&mut self, file: PathBuf, dir: PathBuf) {
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("atd-comfy-import".into())
            .spawn(move || {
                let server = atd_ops_ai::comfy::DEFAULT_SERVER;
                let _ = tx.send(atd_ops_ai::comfy::import(&file, &dir, server, None));
            });
        match spawned {
            Ok(_) => {
                self.comfy_import = Some(rx);
                self.message = Some((
                    format!(
                        "Importing the workflow (reading {})…",
                        atd_ops_ai::comfy::DEFAULT_SERVER
                    ),
                    false,
                ));
            }
            Err(e) => self.message = Some((format!("ComfyUI import: {e}"), true)),
        }
    }
}

/// Runs a dialog future on its own thread.
fn spawn<F>(dialog: F) -> Receiver<Option<PathBuf>>
where
    F: std::future::Future<Output = Option<rfd::FileHandle>> + Send + 'static,
{
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("atd-file-dialog".into())
        .spawn(move || {
            let picked = pollster::block_on(dialog).map(|h| h.path().to_owned());
            let _ = tx.send(picked);
        });
    if let Err(e) = spawned {
        log::error!("cannot open a file dialog: {e}");
    }
    rx
}

/// A file-name-safe version of a project name.
fn file_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.trim_matches('_').is_empty() {
        "untitled".into()
    } else {
        s
    }
}

/// Why a project with Packs cannot be saved from the app yet.
pub const PACKS_READ_ONLY: &str = "This project uses Packs. The app plays it with each Pack shown \
     expanded (its nodes are named instance__node) but cannot save it yet: saving would replace \
     the Packs with their insides. Edit the project files, then check them with `atidarla check`.";

/// A project [`open`] loaded.
#[derive(Debug)]
pub struct Opened {
    /// Its settings (networks live in the engine).
    pub project: Project,
    /// The node to show first.
    pub view: Option<NodeId>,
    /// Its Python operators: registered, watched for hot reload, and
    /// allowed to run only if the user allowed exactly these files before
    /// (docs/python-ops.md); nothing in `ops/` runs here otherwise.
    pub python: atd_py::ops::PythonOps,
    /// Its kernel operators (`kernels/*.atk`): registered and watched for
    /// hot reload.
    pub kernels: atd_ops_pnt::project::ProjectKernels,
    /// Its ComfyUI workflow operators (`comfy/*.json`), from the cached
    /// object_info: opening reaches no server.
    pub comfy: atd_ops_ai::comfy::ProjectComfy,
}

/// Loads the project at `dir` into `engine`.
pub fn open(engine: &mut Engine, dir: &Path) -> Result<Opened, String> {
    let mut project = atd_format::load(dir).map_err(|e| e.to_string())?;
    let mut net = project
        .networks
        .remove(atd_format::MAIN)
        .ok_or("the project has no main network")?;
    let (_, failed) = engine.extend(|r, l| atd_ops_img::isf::register_project_shaders(dir, r, l));
    for (name, e) in failed {
        log::warn!("{}: shader {name} not imported: {e}", dir.display());
    }
    let mut python = atd_py::ops::PythonOps::open(dir);
    // A new project's Script DATs wait for its own choice.
    atd_py::script::allow(false);
    for e in python.errors() {
        log::warn!("{}: {e}", dir.display());
    }
    // Kernel files are read and compiled here, as the project's other files
    // are; saves are compiled on the watcher thread.
    let mut kernels = atd_ops_pnt::project::ProjectKernels::open(dir);
    for e in kernels.errors() {
        log::warn!("{}: {e}", dir.display());
    }
    let comfy = atd_ops_ai::comfy::ProjectComfy::open(dir);
    for e in comfy.errors() {
        log::warn!("{}: {e}", dir.display());
    }
    // Validate against the new project's operators before replacing the
    // old project's, so a project that does not open leaves the patch as it was.
    let mut registry = engine.registry().clone();
    atd_py::ops::clear_custom(&mut registry);
    atd_ops_pnt::project::clear_custom(&mut registry);
    atd_ops_ai::comfy::clear_custom(&mut registry);
    for op in python.ops() {
        let _ = registry.replace_custom(op.schema.clone(), atd_core::registry::PYTHON_CUSTOM);
    }
    for schema in kernels.schemas() {
        let _ = registry.replace_custom(schema.clone(), atd_core::registry::KERNEL_CUSTOM);
    }
    for schema in comfy.schemas() {
        let _ = registry.replace_custom(schema.clone(), atd_core::registry::COMFY_CUSTOM);
    }
    // Pack instances are operators of the project's own Packs.
    let registry = project
        .packs
        .register(&registry)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Err(e) = net.clone().validated(&registry) {
        let why = match &e {
            atd_core::graph::GraphError::UnknownType(t) => python
                .explain(t)
                .or_else(|| kernels.explain(t))
                .or_else(|| comfy.explain(t)),
            _ => None,
        };
        return Err(match why {
            Some(why) => format!("{}: {e}: {why}", dir.display()),
            None => format!("{}: {e}", dir.display()),
        });
    }
    if !python.is_empty()
        && atd_py::ops::TrustStore::user().is_some_and(|s| s.contains(python.hash()))
    {
        python.allow();
    }
    engine.extend(|r, l| python.register(r, l));
    if !python.is_empty()
        && let Err(e) = python.watch()
    {
        log::warn!(
            "{}: Python operators will not hot-reload: {e}",
            dir.display()
        );
    }
    engine.extend(|r, l| kernels.register(r, l));
    engine.extend(|r, l| comfy.register(r, l));
    if let Err(e) = kernels.watch() {
        log::warn!(
            "{}: kernel operators will not hot-reload: {e}",
            dir.display()
        );
    }
    let mut view = None;
    if !project.packs.is_empty() {
        let authored = net
            .validated(&registry)
            .map_err(|e| format!("{}: networks/main.toml: {e}", dir.display()))?;
        let flat = project
            .packs
            .flatten(&authored, &registry)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        view = output_of(&authored, &registry).and_then(|id| flat.view(id));
        net = flat.network;
    }
    let previous = engine.project_dir().map(Path::to_owned);
    engine.set_project_dir(Some(dir.to_owned()));
    if let Err(e) = engine.load(net) {
        engine.set_project_dir(previous);
        return Err(format!("{}: {e}", dir.display()));
    }
    engine.set_resolution(project.resolution[0], project.resolution[1]);
    let view = view.or_else(|| output_node(engine));
    Ok(Opened {
        project,
        view,
        python,
        kernels,
        comfy,
    })
}

/// Saves `engine`'s network as the project at `dir`. Paths to files inside
/// the folder are written relative to it, with `/`, so the project can be
/// moved or opened on another OS.
pub fn save(engine: &Engine, meta: &Project, dir: &Path, store: &[Edit]) -> Result<(), String> {
    let mut net = portable_paths(engine, dir);
    for edit in store {
        net.apply(engine.registry(), edit.clone())
            .map_err(|e| format!("storing mapped values: {e}"))?;
    }
    let mut project = meta.clone();
    project.resolution = [engine.config().width, engine.config().height];
    project.networks.clear();
    project.networks.insert(atd_format::MAIN.to_owned(), net);
    atd_format::save(dir, &project).map_err(|e| e.to_string())
}

/// Edits making relative path parameters absolute (against the current
/// project folder).
fn absolute_paths(engine: &Engine) -> Vec<Edit> {
    let Some(base) = engine.project_dir() else {
        return Vec::new();
    };
    path_edits(engine, engine.network(), |path| {
        (path.is_relative() && !path.as_os_str().is_empty())
            .then(|| base.join(path).to_string_lossy().into_owned())
    })
}

pub(crate) fn portable_paths(engine: &Engine, dir: &Path) -> Network {
    let mut net = engine.network().clone();
    let edits = path_edits(engine, &net, |path| {
        let rel = path.strip_prefix(dir).ok().filter(|_| path.is_absolute())?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(parts.join("/"))
    });
    for edit in edits {
        let _ = net.apply(engine.registry(), edit);
    }
    net
}

/// Edits replacing each path parameter `f` maps to a new path.
fn path_edits(engine: &Engine, net: &Network, f: impl Fn(&Path) -> Option<String>) -> Vec<Edit> {
    let registry = engine.registry();
    let mut edits = Vec::new();
    for node in net.nodes() {
        let Some(schema) = registry.get(&node.type_id) else {
            continue;
        };
        for p in schema.params.iter().filter(|p| p.kind == ParamKind::Path) {
            let Some(param) = node.params.get(&p.name) else {
                continue;
            };
            if let Some(new) = param.value.as_str().and_then(|s| f(Path::new(s))) {
                edits.push(Edit::SetParam {
                    node: node.id,
                    param: p.name.clone(),
                    value: Some(Param {
                        value: Value::from(new),
                        ..param.clone()
                    }),
                });
            }
        }
    }
    edits
}

/// The node to show first: the last IMG node nothing reads from, else the
/// last node.
fn output_node(engine: &Engine) -> Option<NodeId> {
    output_of(engine.network(), engine.registry())
}

fn output_of(net: &Network, registry: &atd_core::Registry) -> Option<NodeId> {
    let read: std::collections::BTreeSet<NodeId> = net.wires().map(|w| w.from.node).collect();
    let is_img = |id: &NodeId| {
        net.node(*id)
            .and_then(|n| registry.get(&n.type_id))
            .and_then(|s| s.outputs.first())
            .is_some_and(|o| o.wire == WireType::Img)
    };
    let ids: Vec<NodeId> = net.nodes().map(|n| n.id).collect();
    ids.iter()
        .rev()
        .copied()
        .find(|id| is_img(id) && !read.contains(id))
        .or_else(|| ids.last().copied())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use atd_core::graph::Node;
    use serde_json::json;

    pub(crate) fn engine() -> Option<Engine> {
        let instance = atd_gpu::primary_instance();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
        let Ok(adapter) = adapter else {
            let required =
                std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return None;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let mut library = atd_gpu::engine::OpLibrary::default();
        atd_ops_img::library(&mut library);
        atd_ops_sig::library(&mut library);
        let mut registry = atd_core::Registry::default();
        atd_ops_img::register(&mut registry);
        atd_ops_sig::register(&mut registry);
        let config = atd_gpu::engine::EngineConfig {
            width: 64,
            height: 64,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        Some(Engine::new(device, queue, registry, library, config))
    }

    fn file_param(engine: &Engine, name: &str) -> String {
        let node = engine.network().node_by_name(name).unwrap();
        node.params["file"].value.as_str().unwrap().to_owned()
    }

    /// The `file` parameter of node `name` in the project saved at `dir`.
    fn saved_file(dir: &Path, name: &str) -> String {
        let project = atd_format::load(dir).unwrap();
        let node = project.main().unwrap().node_by_name(name).unwrap();
        node.params["file"].value.as_str().unwrap().to_owned()
    }

    #[test]
    fn save_writes_portable_paths_and_save_as_keeps_media_found() {
        let Some(mut e) = engine() else { return };
        let root = std::env::temp_dir().join(format!("atd-app-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let first = root.join("first.atidarla");
        std::fs::create_dir_all(first.join("media")).unwrap();
        let inside = first.join("media").join("clip.mov");
        let outside = if cfg!(windows) {
            "C:\\clips\\x.mov"
        } else {
            "/clips/x.mov"
        };
        let (a, b) = (e.alloc_id(), e.alloc_id());
        let set = |id, v: &str| Edit::SetParam {
            node: id,
            param: "file".into(),
            value: Some(Param::constant(json!(v))),
        };
        e.edit(
            "build",
            [
                Edit::AddNode {
                    node: Node::new(a, "img.movie_in", "inside"),
                },
                Edit::AddNode {
                    node: Node::new(b, "img.movie_in", "outside"),
                },
                set(a, &inside.to_string_lossy()),
                set(b, outside),
            ],
        )
        .unwrap();
        e.set_resolution(320, 180);

        // Save: the file inside the project is written relative, with `/`.
        let mut state = ProjectState::untitled(&Network::default());
        state.meta.name = "First".into();
        save(&e, &state.meta, &first, &[]).unwrap();
        let text = std::fs::read_to_string(first.join("networks/main.toml")).unwrap();
        assert!(text.contains(r#"params.file = "media/clip.mov""#), "{text}");
        // (Windows paths are written as TOML literal strings: compare values.)
        assert_eq!(saved_file(&first, "outside"), outside);
        let project = atd_format::load(&first).unwrap();
        assert_eq!(project.resolution, [320, 180]);

        // Open: the relative path stays relative and resolves in the folder.
        let Opened {
            project: meta,
            view,
            python,
            kernels,
            comfy,
        } = open(&mut e, &first).unwrap();
        assert!(python.is_empty() && kernels.is_empty() && comfy.is_empty());
        assert_eq!(meta.name, "First");
        assert_eq!(e.config().width, 320);
        assert_eq!(file_param(&e, "inside"), "media/clip.mov");
        assert_eq!(e.project_dir(), Some(first.as_path()));
        assert!(view.is_some());

        // Save As elsewhere: relative paths become absolute first, so they
        // still point at the first project's media.
        let edits = absolute_paths(&e);
        assert_eq!(edits.len(), 1);
        e.edit("absolute", edits).unwrap();
        // (Joining may leave mixed separators on Windows: compare as paths.)
        assert_eq!(Path::new(&file_param(&e, "inside")), inside);
        let second = root.join("second.atidarla");
        e.set_project_dir(Some(second.clone()));
        save(&e, &meta, &second, &[]).unwrap();
        assert_eq!(Path::new(&saved_file(&second, "inside")), inside);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Opening a project with Python operators loads its patch and
    /// registers them without running them unless they were allowed before
    /// (then the app asks); a missing file is explained and leaves the
    /// current patch alone.
    #[test]
    fn opening_python_operators_loads_without_running_them() {
        let Some(mut e) = engine() else { return };
        let example =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/python-ops.atidarla");
        let mut state = ProjectState::untitled(&Network::default());
        let view = state.run(&mut e, Action::OpenDir(example.clone()));
        assert!(view.is_some(), "{:?}", state.message);
        let python = state.python.as_ref().unwrap();
        assert_eq!(python.ops().count(), 3);
        assert_eq!(state.python_ask, !python.is_allowed());
        assert!(e.registry().get("py.pattern.rings").is_some());
        assert!(e.network().node_by_name("rings1").is_some());

        let dir = std::env::temp_dir().join(format!("atd-app-pyops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        atd_format::save(&dir, &atd_format::load(&example).unwrap()).unwrap();
        let before = e.network().clone();
        state.run(&mut e, Action::OpenDir(dir.clone()));
        let (msg, error) = state.message.clone().unwrap();
        assert!(
            error && msg.contains("ops/motion.py does not exist"),
            "{msg}"
        );
        assert_eq!(*e.network(), before, "the open patch is untouched");
        assert!(
            e.registry().get("py.pattern.rings").is_some(),
            "still registered"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opening a project with kernel files registers each as an operator
    /// whose `ch()` parameters are its controls; another project clears
    /// them.
    #[test]
    fn opening_kernel_files_registers_their_operators() {
        let Some(mut e) = engine() else { return };
        let example =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/kernel-pixels.atidarla");
        let mut state = ProjectState::untitled(&Network::default());
        let view = state.run(&mut e, Action::OpenDir(example));
        let msg = state.message.clone().unwrap();
        assert!(view.is_some() && !msg.1, "{}", msg.0);
        assert!(state.kernels.is_some());
        let grade = e.registry().get("kernel.grade").expect("kernel.grade");
        assert_eq!(
            e.registry().provenance("kernel.grade"),
            Some(atd_core::registry::KERNEL_CUSTOM)
        );
        assert!(grade.params.iter().any(|p| p.name == "vignette"));
        e.cook_frame(view, 0.0).unwrap();
        state.run(&mut e, Action::New);
        assert!(e.registry().get("kernel.grade").is_none());
        assert!(state.kernels.is_none());
    }

    /// Opening the ComfyUI example registers `comfy.invert` from its
    /// cached object_info, without reaching a server; another project
    /// clears it.
    #[test]
    fn opening_comfy_workflows_registers_their_operators() {
        let Some(mut e) = engine() else { return };
        let example =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/comfy-invert.atidarla");
        let mut state = ProjectState::untitled(&Network::default());
        let view = state.run(&mut e, Action::OpenDir(example));
        let msg = state.message.clone().unwrap();
        assert!(view.is_some() && !msg.1, "{}", msg.0);
        assert!(state.comfy.is_some());
        let invert = e.registry().get("comfy.invert").expect("comfy.invert");
        assert_eq!(
            e.registry().provenance("comfy.invert"),
            Some(atd_core::registry::COMFY_CUSTOM)
        );
        assert!(
            invert
                .params
                .iter()
                .any(|p| p.name == "save_image_filename_prefix")
        );
        state.run(&mut e, Action::New);
        assert!(e.registry().get("comfy.invert").is_none());
        assert!(state.comfy.is_none());
        // Importing needs a saved project.
        state.import_comfy();
        assert!(state.message.as_ref().is_some_and(|m| m.1));
    }

    #[test]
    fn open_reports_errors_and_keeps_the_patch() {
        let Some(mut e) = engine() else { return };
        let dir = std::env::temp_dir().join(format!("atd-app-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            e.registry(),
            Edit::AddNode {
                node: Node::new(id, "img.noise", "noise1"),
            },
        )
        .unwrap();
        atd_format::save(&dir, &atd_format::Project::with_main("Bad", net)).unwrap();
        // An operator this build does not have.
        let path = dir.join("networks/main.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("img.noise", "img.nope")).unwrap();

        let before = e.network().clone();
        let mut state = ProjectState::untitled(&before);
        let view = state.run(&mut e, Action::OpenDir(dir.clone()));
        assert!(view.is_none());
        let (msg, error) = state.message.clone().unwrap();
        assert!(error && msg.contains("img.nope"), "{msg}");
        assert_eq!(*e.network(), before, "the open patch is untouched");
        assert!(!state.dirty(&e));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A project with Packs opens with each instance expanded, shows the
    /// Pack's output, and is not saved over (FR-P4, first step in the app).
    #[test]
    fn projects_with_packs_open_expanded_and_refuse_to_save() {
        let Some(mut e) = engine() else { return };
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/pack-chromatic-aberration.atidarla");
        let dir =
            std::env::temp_dir().join(format!("atd-app-packs-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        atd_format::save(&dir, &atd_format::load(&src).unwrap()).unwrap();
        let mut state = ProjectState::untitled(&Network::default());
        let view = state.run(&mut e, Action::OpenDir(dir.clone()));
        let msg = state.message.clone().unwrap();
        assert!(!msg.1, "{}", msg.0);
        let net = e.network();
        assert!(net.node_by_name("aberration1__blue_shift").is_some());
        assert_eq!(
            view.and_then(|v| net.node(v)).map(|n| n.name.as_str()),
            Some("aberration1__add_blue"),
            "the viewer shows the Pack's output"
        );
        e.cook_frame(view, 0.0).unwrap();
        let before = std::fs::read(dir.join("networks/main.toml")).unwrap();
        state.save(&mut e, |_| Vec::new());
        let (msg, error) = state.message.clone().unwrap();
        assert!(error && msg.contains("uses Packs"), "{msg}");
        assert_eq!(
            std::fs::read(dir.join("networks/main.toml")).unwrap(),
            before
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    /// The Models window's "commercial-safe only" toggle is a project edit.
    #[test]
    fn ai_settings_are_unsaved_changes_until_saved() {
        let Some(mut e) = engine() else { return };
        let dir = std::env::temp_dir().join(format!("atd-app-ai-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = ProjectState::untitled(e.network());
        assert!(!state.dirty(&e));
        state.meta.ai.commercial_safe_only = true;
        assert!(state.dirty(&e));
        state.save_to(&mut e, &dir, Vec::new());
        assert!(!state.dirty(&e), "{:?}", state.message);
        assert!(atd_format::load(&dir).unwrap().ai.commercial_safe_only);
        std::fs::remove_dir_all(&dir).ok();
    }
}
