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
}

/// The open project and its unsaved-change state.
#[derive(Debug)]
pub struct ProjectState {
    /// Folder of the open project; `None` until an untitled one is saved.
    pub dir: Option<PathBuf>,
    /// Name, description and resolution (networks live in the engine).
    pub meta: Project,
    /// The network as last opened or saved.
    saved: Network,
    dialog: Option<Dialog>,
    /// An action waiting for "discard unsaved changes?".
    pub confirm: Option<Action>,
    /// The last result to show the user: (text, is an error).
    pub message: Option<(String, bool)>,
}

impl ProjectState {
    /// An untitled project holding `net`, counted as saved.
    pub fn untitled(net: &Network) -> Self {
        Self {
            dir: None,
            meta: Project::new("Untitled"),
            saved: net.clone(),
            dialog: None,
            confirm: None,
            message: None,
        }
    }

    pub fn dirty(&self, engine: &Engine) -> bool {
        *engine.network() != self.saved
    }

    /// The window title: project name, and a dot when there are unsaved
    /// changes.
    pub fn title(&self, engine: Option<&Engine>) -> String {
        let dot = if engine.is_some_and(|e| self.dirty(e)) {
            " •"
        } else {
            ""
        };
        format!("{}{dot} — Atidarla", self.meta.name)
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
                *self = Self::untitled(engine.network());
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
                Ok((meta, view)) => {
                    self.saved = engine.network().clone();
                    self.message = Some((format!("Opened {}", dir.display()), false));
                    self.meta = meta;
                    self.dir = Some(dir);
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

    /// Saves to the project folder, or asks for one first.
    pub fn save(&mut self, engine: &mut Engine) {
        match self.dir.clone() {
            Some(dir) => self.save_to(engine, &dir),
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

    fn save_to(&mut self, engine: &mut Engine, dir: &Path) {
        match save(engine, &self.meta, dir) {
            Ok(()) => {
                self.saved = engine.network().clone();
                self.dir = Some(dir.to_owned());
                self.message = Some((format!("Saved {}", dir.display()), false));
            }
            Err(e) => self.message = Some((e, true)),
        }
    }

    /// Handles a dialog that has been answered. Returns the node to view
    /// after an open.
    pub fn poll(&mut self, engine: &mut Engine) -> Option<NodeId> {
        let (rx, open) = match &self.dialog {
            Some(Dialog::Open(rx)) => (rx, true),
            Some(Dialog::SaveAs(rx)) => (rx, false),
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
            self.save_to(engine, &dir);
            None
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

/// Loads the project at `dir` into `engine`. Returns its settings and the
/// node to show first.
pub fn open(engine: &mut Engine, dir: &Path) -> Result<(Project, Option<NodeId>), String> {
    let mut project = atd_format::load(dir).map_err(|e| e.to_string())?;
    let net = project
        .networks
        .remove(atd_format::MAIN)
        .ok_or("the project has no main network")?;
    let (_, failed) = engine.extend(|r, l| atd_ops_img::isf::register_project_shaders(dir, r, l));
    for (name, e) in failed {
        log::warn!("{}: shader {name} not imported: {e}", dir.display());
    }
    let previous = engine.project_dir().map(Path::to_owned);
    engine.set_project_dir(Some(dir.to_owned()));
    if let Err(e) = engine.load(net) {
        engine.set_project_dir(previous);
        return Err(format!("{}: {e}", dir.display()));
    }
    engine.set_resolution(project.resolution[0], project.resolution[1]);
    let view = output_node(engine);
    Ok((project, view))
}

/// Saves `engine`'s network as the project at `dir`. Paths to files inside
/// the folder are written relative to it, with `/`, so the project can be
/// moved or opened on another OS.
pub fn save(engine: &Engine, meta: &Project, dir: &Path) -> Result<(), String> {
    let net = portable_paths(engine, dir);
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

fn portable_paths(engine: &Engine, dir: &Path) -> Network {
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
    let net = engine.network();
    let read: std::collections::BTreeSet<NodeId> = net.wires().map(|w| w.from.node).collect();
    let is_img = |id: &NodeId| {
        net.node(*id)
            .and_then(|n| engine.registry().get(&n.type_id))
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
mod tests {
    use super::*;
    use atd_core::graph::Node;
    use serde_json::json;

    fn engine() -> Option<Engine> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
        save(&e, &state.meta, &first).unwrap();
        let text = std::fs::read_to_string(first.join("networks/main.toml")).unwrap();
        assert!(text.contains(r#"params.file = "media/clip.mov""#), "{text}");
        assert!(
            text.contains(&format!("params.file = {}", json!(outside))),
            "{text}"
        );
        let project = atd_format::load(&first).unwrap();
        assert_eq!(project.resolution, [320, 180]);

        // Open: the relative path stays relative and resolves in the folder.
        let (meta, view) = open(&mut e, &first).unwrap();
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
        assert_eq!(file_param(&e, "inside"), inside.to_string_lossy());
        let second = root.join("second.atidarla");
        e.set_project_dir(Some(second.clone()));
        save(&e, &meta, &second).unwrap();
        let text = std::fs::read_to_string(second.join("networks/main.toml")).unwrap();
        assert!(text.contains(&format!(
            "params.file = {}",
            json!(inside.to_string_lossy())
        )));
        std::fs::remove_dir_all(&root).ok();
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
}
