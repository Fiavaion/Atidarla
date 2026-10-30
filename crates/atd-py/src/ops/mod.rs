//! Python custom operators (`docs/python-ops.md`; PRD §9 M2, FR-K4).
//!
//! A project's `ops/*.py` files declare SIG and IMG operators with
//! `@atidarla.op(...)`. [`PythonOps`] reads the declarations **without
//! running the files** (Python's `ast`, literals only) and registers them as
//! `py.<file>.<name>` operators with [`PYTHON_CUSTOM`] provenance, so a
//! project loads, shows its operators and saves before anyone decides to run
//! its code. The code runs only after [`PythonOps::allow`], on the worker in
//! [`host`], never on the cook thread (rule 1).

mod host;
mod node;
mod read;
mod trust;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use atd_core::graph::{Edit, Network};
use atd_core::registry::{OpFamily, OperatorSchema, PYTHON_CUSTOM, Registry};
use atd_gpu::engine::OpLibrary;

pub use host::{GRACE, PyOpHost};
pub use trust::TrustStore;

/// The folder of a project that holds its Python operators.
pub const OPS_DIR: &str = "ops";

/// Start of the node error of a Python operator that is not allowed to run.
pub const NOT_ALLOWED: &str = "Python operators in this project are not allowed to run";

/// How often the watcher looks for saved files.
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// One declared operator.
#[derive(Debug, Clone, PartialEq)]
pub struct OpDecl {
    /// `py.<file>.<name>`; the schema the registry holds.
    pub schema: OperatorSchema,
    /// The file's stem (`ops/<file>.py`).
    pub file: String,
    pub name: String,
    /// Line of the `@atidarla.op` decorator.
    pub line: u32,
    /// IMG: output size in pixels.
    pub size: (u32, u32),
    /// How long one cook may take.
    pub timeout: Duration,
    /// The declared state (initial values), for display.
    pub state: serde_json::Map<String, serde_json::Value>,
    /// Parameter and input names, shared with every job.
    pub(crate) param_names: Vec<Arc<str>>,
    pub(crate) input_names: Vec<Arc<str>>,
}

/// A problem with a file in `ops/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsError {
    /// Relative to the project: `ops/<file>.py`.
    pub file: String,
    pub line: Option<u32>,
    pub message: String,
}

impl std::fmt::Display for OpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}, line {line}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

impl std::error::Error for OpsError {}

/// One file in `ops/`, as last read.
#[derive(Debug, Clone)]
pub struct OpsFile {
    /// File name without `.py`.
    pub stem: String,
    /// SHA-256 of the text (line endings normalised).
    pub sha256: String,
    pub ops: Vec<Arc<OpDecl>>,
    /// Why the file's operators cannot be used, if they cannot.
    pub error: Option<OpsError>,
    source: Arc<str>,
    stamp: read::Stamp,
}

/// What a reload changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reload {
    /// Files added, changed or removed (`ops/<file>.py`).
    pub files: Vec<String>,
    /// Errors in the files now (declarations; running them is reported on
    /// their nodes).
    pub errors: Vec<OpsError>,
}

/// A project's Python operators: their declarations, the worker that runs
/// them (once allowed) and, in the app, a watcher for hot reload.
#[derive(Debug)]
pub struct PythonOps {
    dir: PathBuf,
    files: BTreeMap<String, OpsFile>,
    hash: String,
    host: Option<PyOpHost>,
    allowed: bool,
    watcher: Option<Receiver<read::Scan>>,
    stop_watch: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Drop for PythonOps {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop_watch {
            stop.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

impl PythonOps {
    /// Reads the project's `ops/` folder (a missing folder means no
    /// operators). Runs none of it.
    pub fn open(project: &Path) -> Self {
        let scan = read::scan(&project.join(OPS_DIR), &BTreeMap::new());
        let mut ops = Self {
            dir: project.to_owned(),
            files: BTreeMap::new(),
            hash: String::new(),
            host: None,
            allowed: false,
            watcher: None,
            stop_watch: None,
        };
        ops.take(scan);
        ops
    }

    /// No `.py` files in `ops/`.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn files(&self) -> impl Iterator<Item = &OpsFile> {
        self.files.values()
    }

    /// Every declared operator, by file then line.
    pub fn ops(&self) -> impl Iterator<Item = &Arc<OpDecl>> {
        self.files.values().flat_map(|f| f.ops.iter())
    }

    /// Declaration errors, by file.
    pub fn errors(&self) -> Vec<&OpsError> {
        self.files
            .values()
            .filter_map(|f| f.error.as_ref())
            .collect()
    }

    /// SHA-256 over every file in `ops/` (names and texts): what
    /// [`TrustStore`] remembers.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Lets the project's operators run (the user allowed them, or
    /// `--allow-python-ops`). Until then their nodes report [`NOT_ALLOWED`]
    /// and no file in `ops/` is run.
    pub fn allow(&mut self) {
        self.allowed = true;
        if let Some(h) = &self.host {
            h.allow();
        }
    }

    pub fn is_allowed(&self) -> bool {
        self.allowed
    }

    /// The worker, once there are operators.
    pub fn host(&self) -> Option<&PyOpHost> {
        self.host.as_ref()
    }

    /// Why `type_id` (a `py.` operator a network uses) is not registered.
    pub fn explain(&self, type_id: &str) -> Option<String> {
        let (stem, name) = type_id.strip_prefix("py.")?.split_once('.')?;
        Some(match self.files.get(stem) {
            None => format!("{OPS_DIR}/{stem}.py does not exist"),
            Some(OpsFile { error: Some(e), .. }) => e.to_string(),
            Some(_) => format!("{OPS_DIR}/{stem}.py does not define an operator named `{name}`"),
        })
    }

    /// Registers every declared operator (replacing those of a project
    /// opened before) and hands the files to the worker, which runs them
    /// once allowed. Call before loading the network.
    pub fn register(&mut self, registry: &mut Registry, library: &mut OpLibrary) {
        clear_custom(registry);
        self.sync(registry, library);
    }

    /// Starts watching `ops/` for saved files (the app; FR-K4 hot reload).
    /// Changes are read on the watcher thread; [`Self::poll`] applies them.
    pub fn watch(&mut self) -> std::io::Result<()> {
        if self.watcher.is_some() {
            return Ok(());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dir = self.dir.join(OPS_DIR);
        let mut known: BTreeMap<String, OpsFile> = self.files.clone();
        let flag = stop.clone();
        std::thread::Builder::new()
            .name("atd-python-ops-watch".into())
            .spawn(move || {
                while !flag.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(WATCH_EVERY);
                    let scan = read::scan(&dir, &known);
                    // New stamps are remembered even when the text is the
                    // same, so the file is not read again.
                    known.clone_from(&scan.files);
                    if scan.changed && tx.send(scan).is_err() {
                        return;
                    }
                }
            })?;
        self.watcher = Some(rx);
        self.stop_watch = Some(stop);
        Ok(())
    }

    /// Applies files saved since the last call (never blocks: the watcher
    /// thread did the reading). Afterwards apply [`stale_edits`] to the
    /// network.
    pub fn poll(&mut self, registry: &mut Registry, library: &mut OpLibrary) -> Option<Reload> {
        let mut latest = None;
        while let Some(rx) = &self.watcher {
            match rx.try_recv() {
                Ok(scan) => latest = Some(scan),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.watcher = None;
                }
            }
        }
        self.apply(latest?, registry, library)
    }

    /// Reads every file in `ops/` again now and applies what changed
    /// (tests, tools).
    pub fn reload(&mut self, registry: &mut Registry, library: &mut OpLibrary) -> Option<Reload> {
        let scan = read::scan(&self.dir.join(OPS_DIR), &BTreeMap::new());
        self.apply(scan, registry, library)
    }

    fn apply(
        &mut self,
        scan: read::Scan,
        registry: &mut Registry,
        library: &mut OpLibrary,
    ) -> Option<Reload> {
        let mut files: Vec<String> = scan
            .files
            .iter()
            .filter(|(stem, f)| self.files.get(*stem).is_none_or(|o| o.sha256 != f.sha256))
            .chain(
                self.files
                    .iter()
                    .filter(|(stem, _)| !scan.files.contains_key(*stem)),
            )
            .map(|(stem, _)| format!("{OPS_DIR}/{stem}.py"))
            .collect();
        files.sort();
        files.dedup();
        if files.is_empty() {
            // Only times changed: keep the new stamps.
            self.take(scan);
            return None;
        }
        // A file that no longer reads keeps its last good declarations, so
        // its nodes stay valid; the worker is told it is broken.
        let mut scan = scan;
        for (stem, f) in &mut scan.files {
            if f.error.is_some()
                && let Some(old) = self.files.get(stem)
            {
                f.ops.clone_from(&old.ops);
            }
        }
        self.take(scan);
        self.sync(registry, library);
        Some(Reload {
            files,
            errors: self.errors().into_iter().cloned().collect(),
        })
    }

    fn take(&mut self, scan: read::Scan) {
        self.files = scan.files;
        if let Some(hash) = scan.hash {
            self.hash = hash;
        }
    }

    /// Registry, library, worker sources and declarations from `files`.
    fn sync(&mut self, registry: &mut Registry, library: &mut OpLibrary) {
        if self.files.is_empty() && self.host.is_none() {
            return;
        }
        if self.host.is_none() {
            match PyOpHost::start() {
                Ok(h) => {
                    if self.allowed {
                        h.allow();
                    }
                    self.host = Some(h);
                }
                Err(e) => log::error!("cannot start the Python operator worker: {e}"),
            }
        }
        let decls: Vec<Arc<OpDecl>> = self.ops().cloned().collect();
        for decl in &decls {
            let id = decl.schema.type_id.clone();
            if let Err(e) = registry.replace_custom(decl.schema.clone(), PYTHON_CUSTOM) {
                log::warn!("{}: {e}", decl.file);
                continue;
            }
            node::add_factory(library, &id, decl.schema.family, self.host.clone());
        }
        let Some(host) = &self.host else { return };
        host.set_decls(decls);
        let live: BTreeSet<&str> = self.files.keys().map(String::as_str).collect();
        for f in self.files.values() {
            match &f.error {
                None => host.set_source(&f.stem, &f.source),
                Some(e) => host.set_broken(&f.stem, &e.to_string()),
            }
        }
        host.retain_sources(&live);
    }
}

/// Removes every operator registered from Python files (another project is
/// opening). Load a network afterwards.
pub fn clear_custom(registry: &mut Registry) {
    let ids: Vec<String> = registry
        .iter()
        .filter(|s| registry.provenance(&s.type_id) == Some(PYTHON_CUSTOM))
        .map(|s| s.type_id.clone())
        .collect();
    for id in ids {
        registry.remove_custom(&id);
    }
}

/// After a hot reload changed schemas: edits that drop what nodes of Python
/// operators hold but their new schema no longer has, as one undo step, and
/// a note for each ([`atd_core::graph::stale_edits`]).
pub fn stale_edits(net: &Network, registry: &Registry) -> (Vec<Edit>, Vec<String>) {
    atd_core::graph::stale_edits(net, registry, PYTHON_CUSTOM)
}

/// The output wire of a family of Python operator.
fn family_wire(family: OpFamily) -> atd_core::registry::WireType {
    match family {
        OpFamily::Img => atd_core::registry::WireType::Img,
        _ => atd_core::registry::WireType::Sig,
    }
}
