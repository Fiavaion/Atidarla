//! The Python operator worker (`docs/python-ops.md`, CLAUDE.md rule 1).
//!
//! One worker thread per project runs every Python operator's cooks, one
//! at a time; a supervisor thread watches it. Each node owns a [`Slot`]: the
//! cook thread writes the node's job into it and reads the newest result out
//! of it, both in place, so the cook thread neither waits (live) nor
//! allocates once warm. Offline it waits on the slot, never longer than the
//! operator's timeout plus [`GRACE`] plus a margin.
//!
//! **Timeouts.** A cook running past its timeout gets a `TimeoutError`
//! raised inside it (`PyThreadState_SetAsyncExc`). If it is still running
//! [`GRACE`] later (a C call, a long `sleep`), the worker is abandoned: a new
//! worker thread with a fresh Python namespace takes over (every operator's
//! state starts afresh) and the stuck node gets an error. The abandoned
//! thread exits when its cook returns, without writing anything.
//!
//! **Trust gate.** The worker runs no project code until [`PyOpHost::allow`]:
//! before that it only waits.

use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::os::raw::c_long;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::{PyList, PyModule, PyTuple};

use super::OpDecl;

/// How long an interrupted cook may keep running before the worker is
/// restarted.
pub const GRACE: Duration = Duration::from_secs(1);
/// How long running a file's top level may take.
const LOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// The worker re-checks for work at least this often (a missed wake-up
/// costs at most this).
const IDLE_WAIT: Duration = Duration::from_millis(20);
const SUPERVISE_EVERY: Duration = Duration::from_millis(10);

/// The helper module's source (declaration reader and cook host).
const HELPER: &str = include_str!("../ops_host.py");

/// Locks, recovering from a poisoned mutex (a panic elsewhere).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A fresh instance of the helper module under `name`, with the
/// declaration API installed on the embedded `atidarla` module.
pub(crate) fn helper_module<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyModule>> {
    let code = CString::new(HELPER).expect("the helper has no NUL");
    let name = CString::new(name).expect("module names have no NUL");
    let module = PyModule::from_code(py, &code, c"atd_ops_host.py", &name)?;
    module
        .getattr("install")?
        .call1((py.import("atidarla")?,))?;
    Ok(module)
}

/// A parameter value handed to Python.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Float(f64),
    Int(i64),
    Bool(bool),
    Str(String),
    Nums(Vec<f64>),
}

/// One cook's inputs, as the cook thread wrote them.
#[derive(Debug, Default)]
pub(crate) struct Job {
    pub seq: u64,
    pub time: f64,
    pub dt: f64,
    pub frame: u64,
    pub width: u32,
    pub height: u32,
    pub timeout: Duration,
    pub params: Vec<(Arc<str>, Value)>,
    pub inputs: Vec<(Arc<str>, Channels)>,
}

/// A SIG input's channels as (name, latest value).
pub(crate) type Channels = Vec<(String, f64)>;

impl Job {
    fn copy_from(&mut self, o: &Job) {
        self.seq = o.seq;
        self.time = o.time;
        self.dt = o.dt;
        self.frame = o.frame;
        self.width = o.width;
        self.height = o.height;
        self.timeout = o.timeout;
        self.params.clone_from(&o.params);
        self.inputs.clone_from(&o.inputs);
    }
}

/// A cook's result.
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// The job it answers.
    pub seq: u64,
    pub error: Option<String>,
    pub channels: Vec<(String, f32)>,
    pub pixels: Vec<u8>,
    /// Not yet taken by the cook thread.
    pub fresh: bool,
}

/// One node's mailbox.
#[derive(Debug)]
pub(crate) struct Slot {
    pub key: u64,
    pub type_id: Arc<str>,
    /// The newest job and whether the worker has yet to take it.
    pub job: Mutex<(Job, bool)>,
    pub out: Mutex<Output>,
    pub done: Condvar,
}

impl Slot {
    /// Waits until the result of job `seq` (or a later one) is in, at most
    /// until `deadline`. True if it arrived.
    pub(crate) fn wait(&self, seq: u64, deadline: Instant) -> bool {
        let mut out = lock(&self.out);
        while out.seq < seq {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            out = self
                .done
                .wait_timeout(out, left)
                .map_or_else(|e| e.into_inner().0, |(g, _)| g);
        }
        true
    }

    fn answer(&self, seq: u64, result: Answer) {
        let mut out = lock(&self.out);
        out.seq = out.seq.max(seq);
        match result {
            Answer::Channels(ch) => {
                out.channels.clear();
                out.channels
                    .extend(ch.into_iter().map(|(n, v)| (n, v as f32)));
                out.error = None;
            }
            Answer::Pixels(p) => {
                out.pixels.clear();
                out.pixels.extend_from_slice(&p);
                out.error = None;
            }
            Answer::Error(e) => out.error = Some(e),
        }
        out.fresh = true;
        drop(out);
        self.done.notify_all();
    }
}

enum Answer {
    Channels(Vec<(String, f64)>),
    Pixels(PyBackedBytes),
    Error(String),
}

/// What the worker is running, for the supervisor.
#[derive(Debug)]
struct Current {
    /// The node, or `None` while a file loads.
    slot: Option<Weak<Slot>>,
    /// The file being loaded.
    stem: Option<String>,
    seq: u64,
    started: Instant,
    timeout: Duration,
    interrupted: bool,
}

#[derive(Debug, Default)]
struct Running {
    /// Generation of the live worker; a worker whose generation is older
    /// has been abandoned.
    generation: u64,
    /// The live worker's Python thread ID and its `OpTimeout` class.
    ident: Option<c_long>,
    timeout_exc: Option<Py<PyAny>>,
    current: Option<Current>,
}

/// A file as the worker should have it.
#[derive(Debug, Clone, PartialEq)]
enum Content {
    /// Run this text.
    Source(Arc<str>),
    /// Its declarations do not read: do not run it; its nodes report this
    /// and keep their state (and the last code that ran).
    Broken(String),
}

/// The project's files of operators, as the worker should have them.
#[derive(Debug, Default)]
struct Sources {
    /// File stem → (content, version).
    files: BTreeMap<String, (Content, u64)>,
    next_version: u64,
    /// Load result by stem: `None` loaded, `Some(error)` failed.
    loaded: BTreeMap<String, Option<String>>,
    /// Versions whose loading hung: never run again.
    hung: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
struct Shared {
    allowed: AtomicBool,
    stop: AtomicBool,
    /// Mirrors `Running::generation` for quick checks.
    generation: AtomicU64,
    kick: AtomicBool,
    wake_lock: Mutex<()>,
    wake: Condvar,
    slots: Mutex<Vec<(u64, Weak<Slot>)>>,
    sources: Mutex<Sources>,
    running: Mutex<Running>,
    next_key: AtomicU64,
    restarts: AtomicU64,
    decls: Mutex<HashMap<String, Arc<OpDecl>>>,
    decl_gen: AtomicU64,
}

impl Shared {
    fn kick(&self, blocking: bool) {
        self.kick.store(true, Ordering::Release);
        // Holding the lock while notifying means a worker that has just
        // checked `kick` is already waiting. Live, if the worker holds it,
        // it re-checks within IDLE_WAIT.
        let guard = if blocking {
            Some(lock(&self.wake_lock))
        } else {
            self.wake_lock.try_lock().ok()
        };
        self.wake.notify_all();
        drop(guard);
    }
}

/// Stops the threads when the last handle is dropped.
#[derive(Debug)]
struct Token(Arc<Shared>);

impl Drop for Token {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
        self.0.kick(true);
    }
}

/// The Python operator worker of one project. Cheap to clone; the threads
/// stop when the last clone is dropped (they are not joined: a stuck cook
/// must not stall whoever drops the host).
#[derive(Debug, Clone)]
pub struct PyOpHost {
    shared: Arc<Shared>,
    _token: Arc<Token>,
}

impl PyOpHost {
    /// Starts the worker and its supervisor (and the embedded interpreter,
    /// once per process). Nothing from the project runs until [`Self::allow`].
    pub fn start() -> std::io::Result<Self> {
        crate::ensure_initialized();
        let shared = Arc::new(Shared::default());
        spawn_worker(&shared, 0)?;
        let s = shared.clone();
        std::thread::Builder::new()
            .name("atd-python-ops-supervisor".into())
            .spawn(move || supervise(&s))?;
        Ok(Self {
            _token: Arc::new(Token(shared.clone())),
            shared,
        })
    }

    /// Lets the worker run the project's operators (the user allowed them).
    pub fn allow(&self) {
        self.shared.allowed.store(true, Ordering::Release);
        self.shared.kick(true);
    }

    pub fn is_allowed(&self) -> bool {
        self.shared.allowed.load(Ordering::Acquire)
    }

    /// How often the worker was restarted after a cook that would not stop.
    pub fn restarts(&self) -> u64 {
        self.shared.restarts.load(Ordering::Acquire)
    }

    /// The last load result of `ops/<stem>.py`: `None` before it ran,
    /// `Some(None)` loaded, `Some(Some(error))` failed.
    pub fn load_result(&self, stem: &str) -> Option<Option<String>> {
        lock(&self.shared.sources).loaded.get(stem).cloned()
    }

    /// Sets the source of `ops/<stem>.py`; the worker runs it (again) if it
    /// changed, once allowed.
    pub(crate) fn set_source(&self, stem: &str, source: &Arc<str>) {
        self.set_content(stem, Content::Source(source.clone()));
    }

    /// `ops/<stem>.py` no longer reads: its nodes report `message` and stop
    /// cooking, keeping their state, until it is fixed.
    pub(crate) fn set_broken(&self, stem: &str, message: &str) {
        self.set_content(stem, Content::Broken(message.to_owned()));
    }

    fn set_content(&self, stem: &str, content: Content) {
        let mut s = lock(&self.shared.sources);
        if s.files.get(stem).is_some_and(|(old, _)| *old == content) {
            return;
        }
        s.next_version += 1;
        let v = s.next_version;
        s.files.insert(stem.to_owned(), (content, v));
        s.loaded.remove(stem);
        drop(s);
        self.shared.kick(true);
    }

    /// Forgets files not in `stems` (deleted).
    pub(crate) fn retain_sources(&self, stems: &std::collections::BTreeSet<&str>) {
        let mut s = lock(&self.shared.sources);
        let before = s.files.len();
        s.files.retain(|k, _| stems.contains(k.as_str()));
        if s.files.len() != before {
            s.loaded.retain(|k, _| stems.contains(k.as_str()));
            drop(s);
            self.shared.kick(true);
        }
    }

    /// Replaces the declarations nodes read their parameters from.
    pub(crate) fn set_decls(&self, decls: impl IntoIterator<Item = Arc<OpDecl>>) {
        let mut d = lock(&self.shared.decls);
        d.clear();
        d.extend(decls.into_iter().map(|o| (o.schema.type_id.clone(), o)));
        self.shared.decl_gen.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn decl_gen(&self) -> u64 {
        self.shared.decl_gen.load(Ordering::Acquire)
    }

    /// The declaration of `type_id` (`None` if contended or undeclared;
    /// callers retry next frame).
    pub(crate) fn try_decl(&self, type_id: &str) -> Result<Option<Arc<OpDecl>>, ()> {
        let d = self.shared.decls.try_lock().map_err(|_| ())?;
        Ok(d.get(type_id).cloned())
    }

    pub(crate) fn new_slot(&self, type_id: Arc<str>) -> Arc<Slot> {
        let key = self.shared.next_key.fetch_add(1, Ordering::AcqRel) + 1;
        let slot = Arc::new(Slot {
            key,
            type_id,
            job: Mutex::new((Job::default(), false)),
            out: Mutex::new(Output::default()),
            done: Condvar::new(),
        });
        lock(&self.shared.slots).push((key, Arc::downgrade(&slot)));
        slot
    }

    pub(crate) fn kick(&self, blocking: bool) {
        self.shared.kick(blocking);
    }
}

fn spawn_worker(shared: &Arc<Shared>, generation: u64) -> std::io::Result<()> {
    let s = shared.clone();
    std::thread::Builder::new()
        .name(format!("atd-python-ops-{generation}"))
        .spawn(move || worker(&s, generation))
        .map(drop)
}

/// Runs `f` (a call into Python) as the supervised current work. `None` if
/// this worker was abandoned meanwhile.
fn supervised<R>(
    _gil: Python<'_>,
    shared: &Shared,
    generation: u64,
    current: Current,
    f: impl FnOnce() -> R,
) -> Option<R> {
    {
        let mut r = lock(&shared.running);
        if r.generation != generation {
            return None;
        }
        r.current = Some(current);
    }
    let out = f();
    let mut r = lock(&shared.running);
    if r.generation != generation {
        return None;
    }
    if r.current.take().is_some_and(|c| c.interrupted)
        && let Some(ident) = r.ident
    {
        // The interrupt may not have fired inside the cook (it ended just
        // in time): clear it so it does not hit the next one.
        // SAFETY: the GIL is held (`_gil`); a null exception clears the
        // thread's pending asynchronous exception.
        unsafe {
            pyo3::ffi::PyThreadState_SetAsyncExc(ident, std::ptr::null_mut());
        }
    }
    Some(out)
}

fn worker(shared: &Shared, generation: u64) {
    let host: PyResult<Py<PyModule>> = Python::attach(|py| {
        let m = helper_module(py, &format!("atd_ops_host_{generation}"))?;
        let ident: u64 = m.getattr("thread_ident")?.call0()?.extract()?;
        let exc = m.getattr("OpTimeout")?.unbind();
        let mut r = lock(&shared.running);
        if r.generation == generation {
            // Thread IDs are `unsigned long` in C; the FFI declares `long`.
            r.ident = Some(ident as std::os::raw::c_ulong as c_long);
            r.timeout_exc = Some(exc);
        }
        Ok(m.unbind())
    });
    let host = match host {
        Ok(h) => h,
        Err(e) => {
            log::error!("Python operator host: {e}");
            return;
        }
    };
    let mut loaded: HashMap<String, u64> = HashMap::new();
    let mut job = Job::default();
    let mut live: Vec<Arc<Slot>> = Vec::new();
    let mut dead: Vec<u64> = Vec::new();
    loop {
        {
            let mut g = lock(&shared.wake_lock);
            while !shared.kick.swap(false, Ordering::AcqRel)
                && !shared.stop.load(Ordering::Acquire)
                && shared.generation.load(Ordering::Acquire) == generation
            {
                g = shared
                    .wake
                    .wait_timeout(g, IDLE_WAIT)
                    .map_or_else(|e| e.into_inner().0, |(g, _)| g);
            }
        }
        if shared.stop.load(Ordering::Acquire)
            || shared.generation.load(Ordering::Acquire) != generation
        {
            return;
        }
        // The trust gate: no project code before the user allows it.
        if !shared.allowed.load(Ordering::Acquire) {
            continue;
        }
        if load_changed(shared, generation, &host, &mut loaded).is_none() {
            return;
        }
        live.clear();
        dead.clear();
        {
            let mut slots = lock(&shared.slots);
            slots.retain(|(key, w)| match w.upgrade() {
                Some(s) => {
                    live.push(s);
                    true
                }
                None => {
                    dead.push(*key);
                    false
                }
            });
        }
        if !dead.is_empty() {
            Python::attach(|py| {
                for key in &dead {
                    if let Err(e) = host
                        .bind(py)
                        .getattr("forget")
                        .and_then(|f| f.call1((*key,)))
                    {
                        log::warn!("Python operators: {e}");
                    }
                }
            });
        }
        for slot in &live {
            {
                let mut j = lock(&slot.job);
                if !j.1 {
                    continue;
                }
                job.copy_from(&j.0);
                j.1 = false;
            }
            let answer = Python::attach(|py| {
                let current = Current {
                    slot: Some(Arc::downgrade(slot)),
                    stem: None,
                    seq: job.seq,
                    started: Instant::now(),
                    timeout: job.timeout,
                    interrupted: false,
                };
                supervised(py, shared, generation, current, || {
                    cook(py, host.bind(py), slot, &job)
                })
            });
            let Some(answer) = answer else {
                // Abandoned while this cook ran: the new worker owns the
                // slots now.
                return;
            };
            slot.answer(job.seq, answer);
        }
    }
}

/// Runs files that changed since this worker last ran them. `None` if the
/// worker was abandoned.
fn load_changed(
    shared: &Shared,
    generation: u64,
    host: &Py<PyModule>,
    loaded: &mut HashMap<String, u64>,
) -> Option<()> {
    type Todo = Vec<(String, Content, u64)>;
    let (todo, gone, hung): (Todo, Vec<String>, Vec<(String, u64)>) = {
        let s = lock(&shared.sources);
        let todo = s
            .files
            .iter()
            .filter(|(stem, (_, v))| loaded.get(*stem) != Some(v))
            .map(|(stem, (src, v))| (stem.clone(), src.clone(), *v))
            .collect();
        let gone = loaded
            .keys()
            .filter(|stem| !s.files.contains_key(*stem))
            .cloned()
            .collect();
        let hung = s.hung.iter().map(|(k, v)| (k.clone(), *v)).collect();
        (todo, gone, hung)
    };
    for stem in gone {
        loaded.remove(&stem);
        Python::attach(|py| {
            let _ = host
                .bind(py)
                .getattr("unload")
                .and_then(|f| f.call1((&stem,)));
        });
    }
    for (stem, content, version) in todo {
        let filename = format!("ops/{stem}.py");
        let hung_before = hung.iter().any(|(s, v)| *s == stem && *v == version);
        let result: Option<String> = if let Content::Broken(msg) = &content {
            Python::attach(|py| {
                let _ = host
                    .bind(py)
                    .getattr("broken")
                    .and_then(|f| f.call1((&stem, &filename, msg)));
            });
            Some(msg.clone())
        } else if hung_before {
            let msg = format!(
                "running {filename} took longer than {} s; it will not run again until it changes",
                LOAD_TIMEOUT.as_secs()
            );
            // Make the new namespace report it on the nodes too.
            Python::attach(|py| {
                let _ = host
                    .bind(py)
                    .getattr("fail")
                    .and_then(|f| f.call1((&stem, &filename, &msg)));
            });
            Some(msg)
        } else {
            let Content::Source(source) = &content else {
                unreachable!("handled above")
            };
            Python::attach(|py| {
                let current = Current {
                    slot: None,
                    stem: Some(stem.clone()),
                    seq: version,
                    started: Instant::now(),
                    timeout: LOAD_TIMEOUT,
                    interrupted: false,
                };
                supervised(py, shared, generation, current, || {
                    host.bind(py)
                        .getattr("load")
                        .and_then(|f| f.call1((&stem, &**source, &filename)))
                        .and_then(|v| v.extract::<Option<String>>())
                        .unwrap_or_else(|e| Some(format!("{e} (while running {filename})")))
                })
            })?
        };
        if let Some(e) = &result {
            log::warn!("{e}");
        }
        loaded.insert(stem.clone(), version);
        lock(&shared.sources).loaded.insert(stem, result);
    }
    Some(())
}

/// One cook in Python.
fn cook(py: Python<'_>, host: &Bound<'_, PyModule>, slot: &Slot, job: &Job) -> Answer {
    let run = || -> PyResult<Answer> {
        let params = PyList::empty(py);
        for (name, v) in &job.params {
            let value = match v {
                Value::Float(x) => x.into_pyobject(py)?.into_any(),
                Value::Int(x) => x.into_pyobject(py)?.into_any(),
                Value::Bool(x) => x.into_pyobject(py)?.to_owned().into_any(),
                Value::Str(x) => x.into_pyobject(py)?.into_any(),
                Value::Nums(x) => PyTuple::new(py, x)?.into_any(),
            };
            params.append((&**name, value))?;
        }
        let inputs = PyList::empty(py);
        for (name, channels) in &job.inputs {
            inputs.append((&**name, channels))?;
        }
        let out = host.getattr("cook")?.call1((
            slot.key,
            &*slot.type_id,
            job.time,
            job.dt,
            job.frame,
            job.width,
            job.height,
            params,
            inputs,
        ))?;
        let (kind, payload): (String, Bound<'_, PyAny>) = out.extract()?;
        Ok(match kind.as_str() {
            "sig" => Answer::Channels(payload.extract()?),
            "img" => Answer::Pixels(payload.extract()?),
            _ => Answer::Error(payload.extract()?),
        })
    };
    run().unwrap_or_else(|e| {
        // An interrupt that landed outside the operator's own code.
        if e.to_string().starts_with("OpTimeout") {
            Answer::Error(format!(
                "TimeoutError: the cook took longer than its timeout of {} s",
                job.timeout.as_secs_f64()
            ))
        } else {
            Answer::Error(e.to_string())
        }
    })
}

fn supervise(shared: &Arc<Shared>) {
    enum Act {
        Interrupt,
        Restart,
    }
    loop {
        std::thread::sleep(SUPERVISE_EVERY);
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let act = {
            let r = lock(&shared.running);
            r.current.as_ref().and_then(|c| {
                let t = c.started.elapsed();
                if !c.interrupted && t > c.timeout {
                    Some(Act::Interrupt)
                } else if c.interrupted && t > c.timeout + GRACE {
                    Some(Act::Restart)
                } else {
                    None
                }
            })
        };
        match act {
            None => {}
            Some(Act::Interrupt) => Python::attach(|py| {
                // The GIL first, then the lock: the worker takes the lock
                // only while it holds the GIL.
                let mut r = lock(&shared.running);
                let (Some(ident), Some(exc)) =
                    (r.ident, r.timeout_exc.as_ref().map(|e| e.clone_ref(py)))
                else {
                    return;
                };
                if let Some(c) = r.current.as_mut().filter(|c| !c.interrupted) {
                    // SAFETY: the GIL is held; `exc` is a live exception
                    // class, and the call takes its own reference.
                    unsafe {
                        pyo3::ffi::PyThreadState_SetAsyncExc(ident, exc.as_ptr());
                    }
                    c.interrupted = true;
                }
            }),
            Some(Act::Restart) => restart(shared),
        }
    }
}

/// Abandons a worker stuck in a cook that did not stop, and starts another.
fn restart(shared: &Arc<Shared>) {
    let stuck = {
        let mut r = lock(&shared.running);
        let Some(c) = r.current.take() else { return };
        r.generation += 1;
        r.ident = None;
        r.timeout_exc = None;
        shared.generation.store(r.generation, Ordering::Release);
        (c, r.generation)
    };
    let (c, generation) = stuck;
    shared.restarts.fetch_add(1, Ordering::AcqRel);
    let secs = c.timeout.as_secs_f64();
    if let Some(stem) = &c.stem {
        log::error!("Python operators: running ops/{stem}.py did not stop; restarting the worker");
        lock(&shared.sources).hung.insert(stem.clone(), c.seq);
    }
    if let Some(slot) = c.slot.as_ref().and_then(Weak::upgrade) {
        log::error!(
            "Python operator {} did not stop after its timeout; restarting the worker",
            slot.type_id
        );
        slot.answer(
            c.seq,
            Answer::Error(format!(
                "TimeoutError: the cook took longer than its timeout of {secs} s and did not stop \
                 when interrupted, so the Python worker was restarted: every Python operator's \
                 state starts afresh"
            )),
        );
    }
    // Everything runs again in the new namespace.
    lock(&shared.sources).loaded.clear();
    if let Err(e) = spawn_worker(shared, generation) {
        log::error!("cannot restart the Python operator worker: {e}");
    }
    shared.kick(true);
}
