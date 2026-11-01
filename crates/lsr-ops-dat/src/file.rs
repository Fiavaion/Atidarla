//! File In and File Out. Files are read and written on worker threads; the
//! cook only hands work over and picks results up (CLAUDE.md rule 1). An
//! offline render (FR-H1) reads a file on the cook instead, so every
//! frame sees it from the first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use lsr_core::dat::Dat;
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{DatCtx, DatOp};

use crate::{dat_in, delim, op_schema};

/// How often a watched file's modification time is checked.
const WATCH_EVERY: Duration = Duration::from_millis(250);
/// Files larger than this are refused (a DAT is for text, not media).
const MAX_BYTES: u64 = 64 * 1024 * 1024;

const FORMATS: [&str; 4] = ["auto", "text", "csv", "tsv"];

pub fn file_in_schema() -> OperatorSchema {
    let mut s = op_schema(
        "file_in",
        "File In",
        "Reads a text file (up to 64 MB) on a worker thread: as text, or as a table from CSV or TSV. With Watch on it reloads whenever the file is saved; Reload reads it again now. Relative paths are relative to the project folder. Offline renders read it before the first frame.",
        vec![
            ParamSchema::path("file", "File").help("The file to read."),
            ParamSchema::menu("format", "Format", &FORMATS, "auto").help(
                "`auto`: a table for `.csv` and `.tsv`/`.tab` files, text for anything else (feed `.json` to JSON).",
            ),
            ParamSchema::toggle("watch", "Watch", true)
                .help("Reloads the file when it changes on disk."),
            ParamSchema::pulse("reload", "Reload").help("Reads the file again now."),
        ],
        vec![],
        30,
    );
    // Polls its worker every frame (cheap); consumers cook only on change.
    s.time_dependent = true;
    s
}

pub fn file_out_schema() -> OperatorSchema {
    let mut s = op_schema(
        "file_out",
        "File Out",
        "Writes its input to a file on a worker thread, whenever the input changes or on the Write pulse: text as it is, a table as CSV or TSV. Append adds each value to the end of the file (a log). Relative paths are relative to the project folder; missing folders are created. Outputs its input unchanged.",
        vec![
            ParamSchema::path("file", "File").help("The file to write."),
            ParamSchema::menu("format", "Format", &FORMATS, "auto").help(
                "`auto`: CSV for `.csv`, TSV for `.tsv`/`.tab`, otherwise text (a table as TSV).",
            ),
            ParamSchema::menu("mode", "Write", &["on_change", "on_pulse"], "on_change").help(
                "`on_change` writes each new input value; `on_pulse` only on the Write Now pulse.",
            ),
            ParamSchema::pulse("write", "Write Now").help("Writes the current input now."),
            ParamSchema::toggle("append", "Append", false).help(
                "Adds to the end of the file (each value on its own line) instead of replacing it.",
            ),
        ],
        vec![dat_in("in", false)],
        30,
    );
    s.sink = true;
    s
}

/// The format a file is in: `text`, `csv` or `tsv`.
fn format_of(menu: &str, path: &Path) -> &'static str {
    match menu {
        "text" => "text",
        "csv" => "csv",
        "tsv" => "tsv",
        _ => {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.eq_ignore_ascii_case("csv") {
                "csv"
            } else if ext.eq_ignore_ascii_case("tsv") || ext.eq_ignore_ascii_case("tab") {
                "tsv"
            } else {
                "text"
            }
        }
    }
}

fn parse(text: &str, format: &str) -> Dat {
    match format {
        "csv" => Dat::table(delim::parse(text, "comma")),
        "tsv" => Dat::table(delim::parse(text, "tab")),
        _ => Dat::text(text),
    }
}

fn read(path: &Path) -> Result<String, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "{} is {} MB; File In reads up to {} MB",
            path.display(),
            meta.len() / (1024 * 1024),
            MAX_BYTES / (1024 * 1024)
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[derive(Debug, Default)]
struct Loaded {
    seq: u64,
    result: Option<Result<String, String>>,
}

#[derive(Debug, Default)]
struct Shared {
    loaded: Mutex<Loaded>,
    wake: Condvar,
    stop: AtomicBool,
    reload: AtomicBool,
    watch: AtomicBool,
}

/// Reads one file whenever asked or (watching) when it changes.
#[derive(Debug)]
struct Reader {
    shared: Arc<Shared>,
}

impl Reader {
    fn start(path: PathBuf, watch: bool) -> Self {
        let shared = Arc::new(Shared::default());
        shared.watch.store(watch, Ordering::Release);
        let s = shared.clone();
        std::thread::Builder::new()
            .name("lsr-file-in".into())
            .spawn(move || {
                let mut stamp: Option<Option<SystemTime>> = None;
                while !s.stop.load(Ordering::Acquire) {
                    let now = modified(&path);
                    let due = stamp.is_none()
                        || s.reload.swap(false, Ordering::AcqRel)
                        || (s.watch.load(Ordering::Acquire) && stamp != Some(now));
                    if due {
                        stamp = Some(now);
                        let result = read(&path);
                        let mut l = s.loaded.lock().unwrap_or_else(|e| e.into_inner());
                        l.result = Some(result);
                        l.seq += 1;
                    }
                    let l = s.loaded.lock().unwrap_or_else(|e| e.into_inner());
                    drop(s.wake.wait_timeout(l, WATCH_EVERY));
                }
            })
            .expect("spawning a thread");
        Self { shared }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
    }
}

#[derive(Debug, Default)]
struct FileInOp {
    reader: Option<Reader>,
    /// The `file` text the reader was started for.
    file: String,
    format: String,
    seen: u64,
    /// The file's text as last read, kept to re-parse on a format change.
    text: Option<String>,
    error: Option<String>,
    last_reload: bool,
    status: String,
}

impl DatOp for FileInOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let file = p.str("file");
        let format = format_of(p.str("format"), Path::new(file));
        if file != self.file {
            let path = p.resolve(file);
            self.file.clear();
            self.file.push_str(file);
            self.reader = None;
            self.text = None;
            self.error = None;
            self.seen = 0;
            if file.trim().is_empty() {
                self.status.clear();
                ctx.output.replace(Dat::default());
                return Ok(());
            }
            if p.mode().is_offline() {
                // Offline: read now, so the first frame has the file.
                match read(&path) {
                    Ok(t) => self.text = Some(t),
                    Err(e) => self.error = Some(e),
                }
                self.format.clear();
            }
            self.reader = Some(Reader::start(path, p.bool("watch")));
        }
        let Some(reader) = &self.reader else {
            return Ok(());
        };
        reader
            .shared
            .watch
            .store(p.bool("watch"), Ordering::Release);
        let reload = p.bool("reload");
        if reload && !self.last_reload {
            reader.shared.reload.store(true, Ordering::Release);
            reader.shared.wake.notify_all();
        }
        self.last_reload = reload;
        let mut fresh = false;
        if let Ok(mut l) = reader.shared.loaded.try_lock()
            && l.seq != self.seen
        {
            self.seen = l.seq;
            match l.result.take() {
                Some(Ok(t)) => {
                    self.text = Some(t);
                    self.error = None;
                    fresh = true;
                }
                Some(Err(e)) => self.error = Some(e),
                None => {}
            }
        }
        if (fresh || format != self.format)
            && let Some(text) = &self.text
        {
            self.format.clear();
            self.format.push_str(format);
            ctx.output.replace(parse(text, format));
            self.status = format!("{} bytes as {format}", text.len());
        }
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn create_file_in() -> Box<dyn DatOp> {
    Box::<FileInOp>::default()
}

struct Job {
    path: PathBuf,
    bytes: Vec<u8>,
    append: bool,
}

#[derive(Debug, Default)]
struct Written {
    /// Jobs sent and jobs done.
    sent: u64,
    done: u64,
    error: Option<String>,
    status: String,
}

fn write(job: &Job) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = job.path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(job.append)
        .truncate(!job.append)
        .open(&job.path)?;
    f.write_all(&job.bytes)
}

#[derive(Default)]
struct FileOutOp {
    jobs: Option<Sender<Job>>,
    written: Arc<Mutex<Written>>,
    /// The value last written, the parameters it was written with.
    last: Option<Dat>,
    file: String,
    format: String,
    last_pulse: bool,
    status: String,
}

impl std::fmt::Debug for FileOutOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileOutOp")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl FileOutOp {
    fn send(&mut self, job: Job) {
        let tx = self.jobs.get_or_insert_with(|| {
            let (tx, rx) = channel::<Job>();
            let written = self.written.clone();
            std::thread::Builder::new()
                .name("lsr-file-out".into())
                .spawn(move || {
                    for job in rx {
                        let r = write(&job);
                        let mut w = written.lock().unwrap_or_else(|e| e.into_inner());
                        w.done += 1;
                        match r {
                            Ok(()) => {
                                w.error = None;
                                w.status = format!(
                                    "wrote {} bytes to {}",
                                    job.bytes.len(),
                                    job.path.display()
                                );
                            }
                            Err(e) => {
                                w.error = Some(format!("cannot write {}: {e}", job.path.display()))
                            }
                        }
                    }
                })
                .expect("spawning a thread");
            tx
        });
        self.written.lock().unwrap_or_else(|e| e.into_inner()).sent += 1;
        // The worker lives as long as the sender.
        let _ = tx.send(job);
    }
}

impl DatOp for FileOutOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let input = ctx.inputs.get(0).cloned().unwrap_or_default();
        if !Dat::ptr_eq(ctx.output, &input) {
            *ctx.output = input.clone();
        }
        let file = p.str("file");
        let format = format_of(p.str("format"), Path::new(file));
        let pulse = p.bool("write");
        let pulsed = pulse && !self.last_pulse;
        self.last_pulse = pulse;
        if p.mode().is_offline() {
            // A sink offline (docs/render.md): nothing is written.
            return Ok(());
        }
        let changed = self.last.as_ref().is_none_or(|l| !Dat::ptr_eq(l, &input))
            || file != self.file
            || format != self.format;
        let due = pulsed || (p.str("mode") == "on_change" && changed);
        if due && !file.trim().is_empty() {
            let mut text = match format {
                "csv" => delim::write(&input.as_table(), "comma"),
                "tsv" => delim::write(&input.as_table(), "tab"),
                _ => input.as_text().into_owned(),
            };
            let append = p.bool("append");
            if append && !text.ends_with('\n') {
                text.push('\n');
            }
            self.send(Job {
                path: p.resolve(file),
                bytes: text.into_bytes(),
                append,
            });
            self.last = Some(input);
            self.file.clear();
            self.file.push_str(file);
            self.format.clear();
            self.format.push_str(format);
        }
        let w = self.written.lock().unwrap_or_else(|e| e.into_inner());
        // Cook again until the worker reports, to show its result.
        ctx.cook_again = w.done < w.sent;
        if self.status != w.status {
            self.status.clone_from(&w.status);
        }
        match &w.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn create_file_out() -> Box<dyn DatOp> {
    Box::<FileOutOp>::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use lsr_core::dat::Table;
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsr-dat-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Cooks until the output is `want` (the worker reads asynchronously).
    fn wait_for(h: &mut Harness, op: &mut dyn DatOp, want: &str) {
        for _ in 0..2000 {
            let _ = h.cook(op);
            if h.text() == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("never got {want:?}, last {:?}", h.text());
    }

    #[test]
    fn files_are_read_parsed_and_watched() {
        let dir = temp("in");
        std::fs::write(dir.join("words.csv"), "a,b\nc,d\n").unwrap();
        let mut h = Harness::new(file_in_schema());
        h.dir = Some(dir.clone());
        let mut op = create_file_in();
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "", "no file: empty, no error");
        h.set("file", json!("words.csv"));
        wait_for(&mut h, op.as_mut(), "a\tb\nc\td");
        assert!(!h.out.is_text());
        assert!(op.status().unwrap().contains("csv"));
        // Saved again: reloaded.
        std::fs::write(dir.join("words.csv"), "x,y\n").unwrap();
        wait_for(&mut h, op.as_mut(), "x\ty");
        // A format change re-parses what was read.
        h.set("format", json!("text"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "x,y\n");
        assert!(h.out.is_text());
        // A missing file is an error.
        h.set("file", json!("missing.txt"));
        let mut err = None;
        for _ in 0..2000 {
            if let Err(e) = h.cook(op.as_mut()) {
                err = Some(e);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(err.unwrap().contains("missing.txt"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_renders_read_before_the_first_frame() {
        let dir = temp("offline");
        std::fs::write(dir.join("t.txt"), "hello").unwrap();
        let mut h = Harness::new(file_in_schema());
        h.set("file", json!(dir.join("t.txt").to_string_lossy()));
        let mut op = create_file_in();
        // Cook through the engine's offline mode.
        let params = lsr_gpu::engine::Params::new(&h.net, &h.registry, &h.sig, h.node)
            .with_mode(lsr_gpu::engine::CookMode::Offline { fps: 30.0 });
        h.out = Dat::default();
        let mut ctx = DatCtx {
            inputs: lsr_gpu::engine::DatInputs::none(),
            sig_inputs: lsr_gpu::engine::SigInputs::new(&[], &h.sig),
            output: &mut h.out,
            params,
            inputs_changed: true,
            time: 0.0,
            dt: 0.0,
            frame: 0,
            cook_again: false,
        };
        op.cook(&mut ctx).unwrap();
        assert_eq!(h.out.as_text(), "hello");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn values_are_written_on_change_on_pulse_and_appended() {
        let dir = temp("out");
        let mut h = Harness::new(file_out_schema());
        h.dir = Some(dir.clone());
        let mut op = create_file_out();
        let table = Dat::table(Table::from_rows([["a", "b,c"]]));
        h.input(0, Some(table.clone()))
            .set("file", json!("sub/out.csv"));
        h.settle(op.as_mut(), 2000).unwrap();
        assert!(Dat::ptr_eq(&h.out, &table), "passes its input through");
        assert_eq!(
            std::fs::read_to_string(dir.join("sub/out.csv")).unwrap(),
            "a,\"b,c\"\n"
        );
        assert!(op.status().unwrap().contains("wrote"));
        // Append mode: a log of text values.
        h.set("file", json!("log.txt"))
            .set("append", json!(true))
            .input(0, Some(Dat::text("one")));
        h.settle(op.as_mut(), 2000).unwrap();
        h.input(0, Some(Dat::text("two")));
        h.settle(op.as_mut(), 2000).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log.txt")).unwrap(),
            "one\ntwo\n"
        );
        // On pulse only: a new value is not written until the pulse.
        h.set("mode", json!("on_pulse"))
            .set("append", json!(false))
            .input(0, Some(Dat::text("three")));
        h.settle(op.as_mut(), 2000).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log.txt")).unwrap(),
            "one\ntwo\n"
        );
        h.set("write", json!(true));
        h.settle(op.as_mut(), 2000).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log.txt")).unwrap(),
            "three"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
