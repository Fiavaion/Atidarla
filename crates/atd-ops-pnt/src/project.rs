//! A project's kernel operators (FR-K1, `docs/kernel-dsl.md`): every
//! `kernels/<name>.atk` (Points) and `kernels/<name>.pixels.atk` (Pixels)
//! file is an operator type `kernel.<name>` with [`KERNEL_CUSTOM`]
//! provenance whose parameters are the file's `ch()` declarations.
//!
//! [`ProjectKernels`] reads and compiles the files when the project opens
//! and, once [`ProjectKernels::watch`] runs, again on its own thread whenever
//! one is saved; [`ProjectKernels::poll`] applies what that thread read
//! without touching the disk (rule 1). A node of a kernel operator reads the
//! file's latest text through a [`SharedSource`] and compiles it on its own
//! worker, as `pnt.kernel` and `img.kernel` do, so the last kernel that
//! compiled keeps running and a simulation keeps its state (FR-K4). After a
//! save changed a schema, apply [`stale_edits`] to the network: it keeps
//! every parameter value whose name and kind still fit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, SystemTime};

use atd_core::graph::{Edit, Network};
use atd_core::registry::{KERNEL_CUSTOM, OperatorSchema, ParamSchema, Registry};
use atd_gpu::engine::OpLibrary;
use atd_kernel::{CompileOptions, ParamDecl, ParamType, Target};
use atd_ops_img::kernel::{SharedSource, Text, pixels_schema};

/// The folder of a project that holds its kernel files.
pub const KERNELS_DIR: &str = "kernels";

/// Prefix of the type IDs of project kernel operators.
pub const TYPE_PREFIX: &str = "kernel.";

/// How often the watcher looks for saved files.
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// A problem with a file in `kernels/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelError {
    /// Relative to the project: `kernels/<file>`.
    pub file: String,
    pub line: Option<u32>,
    pub message: String,
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}, line {line}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

impl std::error::Error for KernelError {}

/// What a reload changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reload {
    /// Files added, changed or removed (`kernels/<file>`).
    pub files: Vec<String>,
    /// Errors in the files now.
    pub errors: Vec<KernelError>,
}

/// What identifies a file's version cheaply: modification time and size.
type Stamp = Option<(SystemTime, u64)>;

/// One file in `kernels/`, as last read.
#[derive(Debug, Clone)]
struct Entry {
    /// The operator's name (`curl` for `curl.atk` and `curl.pixels.atk`).
    name: String,
    target: Target,
    stamp: Stamp,
    text: Text,
    /// The operator this text declares; `None` when it does not compile.
    schema: Option<OperatorSchema>,
    error: Option<KernelError>,
    /// The file defines operator `kernel.<name>`: its name is valid and no
    /// file before it (by file name) has the same name.
    defines: bool,
}

/// A registered operator: its schema, and the text its nodes run.
#[derive(Debug)]
struct Op {
    file: String,
    target: Target,
    schema: OperatorSchema,
    shared: Arc<SharedSource>,
    /// The text last handed to `shared`.
    text: Text,
}

/// A project's kernel operators.
#[derive(Debug)]
pub struct ProjectKernels {
    dir: PathBuf,
    /// By file name.
    entries: BTreeMap<String, Entry>,
    /// By operator name. A file that no longer compiles, or was removed,
    /// keeps its last good schema, so its nodes stay valid.
    ops: BTreeMap<String, Op>,
    watcher: Option<Receiver<BTreeMap<String, Entry>>>,
    stop_watch: Option<Arc<AtomicBool>>,
}

impl Drop for ProjectKernels {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop_watch {
            stop.store(true, Ordering::Release);
        }
    }
}

/// The operator name and target of a kernel file name, if it is one.
fn split_name(file: &str) -> Option<(&str, Target)> {
    if let Some(name) = file.strip_suffix(".pixels.atk") {
        return Some((name, Target::Pixels));
    }
    file.strip_suffix(".atk").map(|n| (n, Target::Points))
}

/// Lower-case ASCII letters, digits and `_`, starting with a letter or `_`.
fn valid_name(name: &str) -> bool {
    let mut b = name.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// The shortest decimal that reads back as `x` (so `0.3` is `0.3`, not
/// `0.30000001192092896`, in schemas and project files).
fn short(x: f32) -> f64 {
    x.to_string().parse().unwrap_or(f64::from(x))
}

/// `spin_rate` → `Spin rate`.
fn label(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut c = words.trim().chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => name.to_owned(),
    }
}

/// The text of a `//` comment after `span` on its line, if there is one.
fn line_comment(source: &str, span: atd_kernel::Span) -> Option<&str> {
    let end = span.end.min(source.len());
    let line_end = source[end..].find('\n').map_or(source.len(), |i| end + i);
    let rest = &source[end..line_end];
    let text = rest[rest.find("//")? + 2..].trim();
    (!text.is_empty()).then_some(text)
}

/// The control for one `ch()` declaration.
fn param(decl: &ParamDecl, source: &str, file: &str) -> ParamSchema {
    let name = decl.name.as_str();
    let label = label(name);
    let schema = match decl.ty {
        ParamType::Float => ParamSchema::float(
            name,
            &label,
            short(decl.default[0]),
            decl.min.map(short),
            decl.max.map(short),
        ),
        ParamType::Int => ParamSchema::int(
            name,
            &label,
            decl.default[0] as i64,
            decl.min.map(|x| x as i64),
            decl.max.map(|x| x as i64),
        ),
        ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4 => {
            let n = decl.ty.components();
            let d: Vec<f64> = decl.default[..n].iter().map(|x| short(*x)).collect();
            ParamSchema::vec(name, &label, &d)
        }
    };
    match line_comment(source, decl.span) {
        Some(help) => schema.help(help),
        None => schema.help(&format!("`ch(\"{name}\")` in {KERNELS_DIR}/{file}.")),
    }
}

/// The file's leading `//` comment lines, joined, if it starts with one.
fn leading_comment(source: &str) -> Option<String> {
    let lines: Vec<&str> = source
        .lines()
        .map_while(|l| l.trim().strip_prefix("//"))
        .map(str::trim)
        .collect();
    let text = lines.join(" ");
    (!text.trim().is_empty()).then_some(text)
}

/// The operator a kernel file declares, or its first error.
fn declare(
    file: &str,
    name: &str,
    target: Target,
    source: &str,
) -> Result<OperatorSchema, KernelError> {
    let at = |line, message: String| KernelError {
        file: format!("{KERNELS_DIR}/{file}"),
        line,
        message,
    };
    // Parameters do not depend on the input's attributes; the nodes compile
    // again against their input.
    let mut opts = CompileOptions::new(target);
    opts.validate = false;
    let compiled = atd_kernel::compile(source, &opts).map_err(|ds| {
        let d = &ds[0];
        at(Some(d.line), format!("{}:{}: {}", d.line, d.col, d.message))
    })?;
    let mut params = Vec::with_capacity(compiled.params.len());
    for d in &compiled.params {
        let p = param(d, source, file);
        if let Err(e) = p.validate(&p.default) {
            let line = source[..d.span.start.min(source.len())]
                .lines()
                .count()
                .max(1);
            return Err(at(Some(line as u32), format!("`{}`: {e}", d.name)));
        }
        params.push(p);
    }
    let what = match target {
        Target::Points => "a Points kernel: runs once for every point of its input",
        Target::Pixels => "a Pixels kernel: runs once for every output pixel",
    };
    let mut summary = format!("The project's `{KERNELS_DIR}/{file}`, {what}.");
    if let Some(c) = leading_comment(source) {
        summary = format!("{c} ({summary})");
    }
    let type_id = format!("{TYPE_PREFIX}{name}");
    let label = label(name);
    let mut schema = match target {
        Target::Points => {
            let mut s = crate::pnt(name, &label, &summary, &[("in", false)], params, 150);
            s.time_dependent = true;
            s
        }
        Target::Pixels => pixels_schema(&type_id, &label, &summary, params),
    };
    schema.type_id = type_id;
    Ok(schema)
}

/// Reads and declares one file.
fn read_entry(dir: &Path, file: &str, name: &str, target: Target, stamp: Stamp) -> Entry {
    let text: Text = std::fs::read_to_string(dir.join(file))
        .map(|t| Arc::from(t.as_str()))
        .map_err(|e| format!("{KERNELS_DIR}/{file}: {e}"));
    let (schema, error) = match &text {
        Ok(source) => match declare(file, name, target, source) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e)),
        },
        Err(e) => (
            None,
            Some(KernelError {
                file: format!("{KERNELS_DIR}/{file}"),
                line: None,
                message: e.clone(),
            }),
        ),
    };
    Entry {
        name: name.to_owned(),
        target,
        stamp,
        text,
        schema,
        error,
        defines: true,
    }
}

/// Reads `dir` (a project's `kernels/`), re-reading only files whose stamp
/// differs from `known`. A missing folder holds no kernels.
fn scan(dir: &Path, known: &BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    let mut entries = BTreeMap::new();
    let Ok(list) = std::fs::read_dir(dir) else {
        return entries;
    };
    for item in list.flatten() {
        let Some(file) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((name, target)) = split_name(&file) else {
            continue;
        };
        if !item.path().is_file() {
            continue;
        }
        let stamp = item
            .metadata()
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        let entry = match known.get(&file) {
            Some(old) if old.stamp.is_some() && old.stamp == stamp => old.clone(),
            _ if !valid_name(name) => Entry {
                name: name.to_owned(),
                target,
                stamp,
                text: Err("not read".into()),
                schema: None,
                error: Some(KernelError {
                    file: format!("{KERNELS_DIR}/{file}"),
                    line: None,
                    message: "kernel file names are lower-case letters, digits and _ \
                              (they name the operator `kernel.<name>`)"
                        .into(),
                }),
                defines: false,
            },
            _ => read_entry(dir, &file, name, target, stamp),
        };
        entries.insert(file, entry);
    }
    // Two files for one name: the first (by file name) defines it.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (file, e) in &mut entries {
        if !valid_name(&e.name) {
            continue;
        }
        match seen.get(&e.name) {
            None => {
                e.defines = true;
                seen.insert(e.name.clone(), file.clone());
            }
            Some(first) => {
                e.defines = false;
                e.schema = None;
                e.error = Some(KernelError {
                    file: format!("{KERNELS_DIR}/{file}"),
                    line: None,
                    message: format!(
                        "{KERNELS_DIR}/{first} already defines `{TYPE_PREFIX}{}`",
                        e.name
                    ),
                });
            }
        }
    }
    entries
}

impl ProjectKernels {
    /// Reads and compiles the project's `kernels/` files (a missing folder
    /// means no kernels).
    pub fn open(project: &Path) -> Self {
        let mut kernels = Self {
            dir: project.to_owned(),
            entries: BTreeMap::new(),
            ops: BTreeMap::new(),
            watcher: None,
            stop_watch: None,
        };
        let entries = scan(&project.join(KERNELS_DIR), &BTreeMap::new());
        kernels.take(entries);
        kernels
    }

    /// No kernel files.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every operator's schema, by name.
    pub fn schemas(&self) -> impl Iterator<Item = &OperatorSchema> {
        self.ops.values().map(|o| &o.schema)
    }

    /// Errors in the files, by file.
    pub fn errors(&self) -> Vec<&KernelError> {
        self.entries
            .values()
            .filter_map(|e| e.error.as_ref())
            .collect()
    }

    /// Why `type_id` (a `kernel.` operator a network uses) is not registered.
    pub fn explain(&self, type_id: &str) -> Option<String> {
        let name = type_id.strip_prefix(TYPE_PREFIX)?;
        let errors: Vec<String> = self
            .entries
            .values()
            .filter(|e| e.name == name)
            .filter_map(|e| e.error.as_ref().map(ToString::to_string))
            .collect();
        Some(match errors.first() {
            Some(e) => e.clone(),
            None => format!(
                "neither {KERNELS_DIR}/{name}.atk nor {KERNELS_DIR}/{name}.pixels.atk exists"
            ),
        })
    }

    /// Registers every operator (replacing those of a project opened
    /// before). Call before loading the network.
    pub fn register(&self, registry: &mut Registry, library: &mut OpLibrary) {
        clear_custom(registry);
        self.sync(registry, library);
    }

    /// Starts watching `kernels/` for saved files (the app; FR-K4 hot
    /// reload). Files are read and compiled on the watcher thread;
    /// [`Self::poll`] applies them.
    pub fn watch(&mut self) -> std::io::Result<()> {
        if self.watcher.is_some() {
            return Ok(());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let dir = self.dir.join(KERNELS_DIR);
        let mut known = self.entries.clone();
        let flag = stop.clone();
        std::thread::Builder::new()
            .name("atd-kernels-watch".into())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    std::thread::sleep(WATCH_EVERY);
                    let now = scan(&dir, &known);
                    let changed = now.len() != known.len()
                        || now
                            .iter()
                            .zip(&known)
                            .any(|((f, a), (g, b))| f != g || a.stamp != b.stamp);
                    known.clone_from(&now);
                    if changed && tx.send(now).is_err() {
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
                Ok(entries) => latest = Some(entries),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.watcher = None,
            }
        }
        self.apply(latest?, registry, library)
    }

    /// Reads every file in `kernels/` again now and applies what changed
    /// (tests, tools).
    pub fn reload(&mut self, registry: &mut Registry, library: &mut OpLibrary) -> Option<Reload> {
        let entries = scan(&self.dir.join(KERNELS_DIR), &BTreeMap::new());
        self.apply(entries, registry, library)
    }

    fn apply(
        &mut self,
        entries: BTreeMap<String, Entry>,
        registry: &mut Registry,
        library: &mut OpLibrary,
    ) -> Option<Reload> {
        let mut files: Vec<String> = entries
            .iter()
            .filter(|(f, e)| self.entries.get(*f).is_none_or(|o| o.text != e.text))
            .map(|(f, _)| f)
            .chain(self.entries.keys().filter(|f| !entries.contains_key(*f)))
            .map(|f| format!("{KERNELS_DIR}/{f}"))
            .collect();
        files.sort();
        files.dedup();
        self.take(entries);
        if files.is_empty() {
            return None;
        }
        self.sync(registry, library);
        Some(Reload {
            files,
            errors: self.errors().into_iter().cloned().collect(),
        })
    }

    /// Operators and their texts from `entries`.
    fn take(&mut self, entries: BTreeMap<String, Entry>) {
        for (file, e) in &entries {
            if !e.defines {
                continue;
            }
            match self.ops.get_mut(&e.name) {
                Some(op) => {
                    if op.target != e.target {
                        // Another target: a new operator; nodes of the old
                        // one keep running their last kernel.
                        op.shared = SharedSource::new(e.text.clone());
                        op.target = e.target;
                    } else if op.text != e.text {
                        op.shared.set(e.text.clone());
                    }
                    op.file.clone_from(file);
                    op.text = e.text.clone();
                    if let Some(s) = &e.schema {
                        op.schema = s.clone();
                    }
                }
                None => {
                    let Some(schema) = &e.schema else { continue };
                    self.ops.insert(
                        e.name.clone(),
                        Op {
                            file: file.clone(),
                            target: e.target,
                            schema: schema.clone(),
                            shared: SharedSource::new(e.text.clone()),
                            text: e.text.clone(),
                        },
                    );
                }
            }
        }
        for op in self.ops.values_mut() {
            if !entries.contains_key(&op.file) && op.text.is_ok() {
                op.text = Err(format!("{KERNELS_DIR}/{} was removed", op.file));
                op.shared.set(op.text.clone());
            }
        }
        self.entries = entries;
    }

    /// Registry and library from the operators.
    fn sync(&self, registry: &mut Registry, library: &mut OpLibrary) {
        for op in self.ops.values() {
            let id = op.schema.type_id.clone();
            if let Err(e) = registry.replace_custom(op.schema.clone(), KERNEL_CUSTOM) {
                log::warn!("{KERNELS_DIR}/{}: {e}", op.file);
                continue;
            }
            let shared = op.shared.clone();
            match op.target {
                Target::Points => library.add_geo(id, move |device| {
                    crate::kernel::create_project(device, shared.clone())
                }),
                Target::Pixels => library.add(id, move |device, format| {
                    atd_ops_img::kernel::create_project(device, format, shared.clone())
                }),
            }
        }
    }
}

/// Removes every operator registered from kernel files (another project is
/// opening). Load a network afterwards.
pub fn clear_custom(registry: &mut Registry) {
    let ids: Vec<String> = registry
        .iter()
        .filter(|s| registry.provenance(&s.type_id) == Some(KERNEL_CUSTOM))
        .map(|s| s.type_id.clone())
        .collect();
    for id in ids {
        registry.remove_custom(&id);
    }
}

/// After a reload changed schemas: edits that drop what nodes of kernel
/// operators hold but their new schema no longer has (a removed or retyped
/// `ch()`), as one undo step, and a note for each
/// ([`atd_core::graph::stale_edits`]).
pub fn stale_edits(net: &Network, registry: &Registry) -> (Vec<Edit>, Vec<String>) {
    atd_core::graph::stale_edits(net, registry, KERNEL_CUSTOM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_labels_and_comments() {
        assert_eq!(split_name("curl.atk"), Some(("curl", Target::Points)));
        assert_eq!(
            split_name("grade.pixels.atk"),
            Some(("grade", Target::Pixels))
        );
        assert_eq!(split_name("notes.txt"), None);
        assert!(valid_name("curl_2") && !valid_name("Curl") && !valid_name("2x"));
        assert_eq!(label("spin_rate"), "Spin rate");
        assert_eq!(short(0.3), 0.3);
        let src = "float a = ch(\"a\", 1); // How far.\nfloat b = ch(\"b\");\n";
        let span = |s: usize, e: usize| atd_kernel::Span { start: s, end: e };
        assert_eq!(line_comment(src, span(10, 22)), Some("How far."));
        assert_eq!(line_comment(src, span(43, 50)), None);
        assert_eq!(
            leading_comment("// One.\n// Two.\nx;").as_deref(),
            Some("One. Two.")
        );
    }

    #[test]
    fn declarations_become_controls() {
        let src = "// Spins points.\n\
                   float rate = ch(\"spin_rate\", 0.3, 0, 2); // Turns per second.\n\
                   int n = chi(\"steps\", 4, 1, 16);\n\
                   vec3 axis = chv(\"axis\", {0, 1, 0});\n\
                   vec4 tint = chp(\"tint\", {1, 0.5, 0.25, 1});\n\
                   @P += axis * rate * float(n) + tint.xyz * 0.0;\n";
        let s = declare("spin.atk", "spin", Target::Points, src).unwrap();
        assert_eq!(s.type_id, "kernel.spin");
        assert_eq!(s.family, atd_core::registry::OpFamily::Pnt);
        assert!(s.summary.starts_with("Spins points."), "{}", s.summary);
        let names: Vec<&str> = s.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["spin_rate", "steps", "axis", "tint"]);
        assert_eq!(
            s.params[0],
            ParamSchema::float("spin_rate", "Spin rate", 0.3, Some(0.0), Some(2.0))
                .help("Turns per second.")
        );
        assert_eq!(
            s.params[1],
            ParamSchema::int("steps", "Steps", 4, Some(1), Some(16))
                .help("`ch(\"steps\")` in kernels/spin.atk.")
        );
        assert_eq!(s.params[2].default, serde_json::json!([0.0, 1.0, 0.0]));
        assert_eq!(
            s.params[3].default,
            serde_json::json!([1.0, 0.5, 0.25, 1.0])
        );
        let e = declare("bad.atk", "bad", Target::Points, "\n@P += nope;\n").unwrap_err();
        assert_eq!(e.line, Some(2));
        assert!(e.message.starts_with("2:"), "{e}");
        let e = declare(
            "r.atk",
            "r",
            Target::Points,
            "float x = ch(\"x\", 5, 0, 1);\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("`x`"), "{e}");
    }
}
