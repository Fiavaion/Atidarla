//! Script DAT (`dat.script`): a `cook(inputs)` function in Python that
//! turns DAT inputs into a DAT value. The schema is registered with the
//! DAT family (`atd-ops-dat`); this is its implementation, added to the
//! engine's library with [`library`].
//!
//! Scripts run on one worker thread shared by every Script node, never on
//! the cook thread (CLAUDE.md rule 1): a node hands its code and inputs
//! over when either changes and picks the result up on a later frame (an
//! offline render waits for it). A script is project code, so it runs only
//! while exactly this node's code is allowed ([`crate::allow`],
//! `docs/python-ops.md`); until then the node reports
//! [`NOT_ALLOWED`](crate::ops::NOT_ALLOWED). The worker checks again before
//! it runs a job.
//!
//! **Timeouts** work as for Python operators (`ops::host`): a supervisor
//! thread raises a `TimeoutError` inside a run that passes its node's
//! `timeout_s` (`PyThreadState_SetAsyncExc`, from a short-lived thread so
//! the supervisor never waits for the GIL). If the run is still going
//! [`GRACE`] later (a blocking C call, a long `sleep`, a script that
//! swallows the exception), its node gets the error and the worker is
//! abandoned for a new one with a fresh namespace; the abandoned thread
//! exits, writing nothing, if its call ever returns. A C call that holds
//! the GIL cannot be interrupted at all: Python waits for it, but the node
//! still reports the timeout and the cook thread never waits.

use std::collections::VecDeque;
use std::ffi::CString;
use std::os::raw::{c_long, c_ulong};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use atd_core::graph::NodeId;
use atd_gpu::engine::{Dat, DatCtx, DatOp, OpLibrary, Table};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyModule};

/// The operator type this implements.
pub const TYPE_ID: &str = "dat.script";

/// How long an offline render waits for a script whose timeout is 0 (none).
pub const OFFLINE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an interrupted script may keep running before its worker is
/// replaced.
pub const GRACE: Duration = Duration::from_secs(1);
/// The longest timeout (the parameter's maximum).
const MAX_TIMEOUT_S: f64 = 600.0;
const SUPERVISE_EVERY: Duration = Duration::from_millis(10);
/// The worker re-checks whether it was abandoned at least this often.
const IDLE_WAIT: Duration = Duration::from_millis(100);

const HELPER: &str = r#"
import threading

_fns = {}

class ScriptTimeout(TimeoutError):
    """Raised inside a script that ran past its timeout (by the supervisor)."""

def thread_ident():
    return threading.get_ident()

def _cell(v):
    return "" if v is None else str(v)

def _line(e):
    if isinstance(e, SyntaxError) and e.filename == "<script>":
        return e.lineno
    line, tb = None, e.__traceback__
    while tb is not None:
        if tb.tb_frame.f_code.co_filename == "<script>":
            line = tb.tb_lineno
        tb = tb.tb_next
    return line

def run(source, inputs):
    try:
        return _run(source, inputs)
    except ScriptTimeout as e:
        return ("timeout", _line(e) or 0)
    except Exception as e:
        line = _line(e)
        where = f" (line {line})" if line else ""
        return ("error", f"{type(e).__name__}: {e}{where}")

def _run(source, inputs):
    fn = _fns.get(source)
    if fn is None:
        ns = {"__name__": "atd_script"}
        exec(compile(source, "<script>", "exec"), ns)
        fn = ns.get("cook")
        if not callable(fn):
            raise NameError("the script defines no cook(inputs) function")
        if len(_fns) > 64:
            _fns.clear()
        _fns[source] = fn
    out = fn(inputs)
    if out is None:
        return ("text", "")
    if isinstance(out, str):
        return ("text", out)
    if isinstance(out, dict):
        return ("table", [[_cell(k), _cell(v)] for k, v in out.items()])
    rows = []
    for item in out:
        if isinstance(item, (list, tuple)):
            rows.append([_cell(c) for c in item])
        else:
            rows.append([_cell(item)])
    return ("table", rows)
"#;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where one node's result arrives.
#[derive(Debug, Default)]
struct Slot {
    result: Mutex<Option<Result<Dat, String>>>,
    done: Condvar,
}

impl Slot {
    fn answer(&self, result: Result<Dat, String>) {
        *lock(&self.result) = Some(result);
        self.done.notify_all();
    }
}

struct Job {
    node: NodeId,
    code: String,
    inputs: Vec<Option<Dat>>,
    /// `None`: no limit.
    timeout: Option<Duration>,
    slot: Arc<Slot>,
}

/// The run the worker is in, for the supervisor.
struct Current {
    /// Tells this run from the next one.
    seq: u64,
    started: Instant,
    timeout: Duration,
    interrupted: bool,
    slot: Arc<Slot>,
}

#[derive(Default)]
struct Running {
    /// Generation of the live worker; an older one has been abandoned.
    generation: u64,
    /// The live worker's Python thread ID and its `ScriptTimeout` class.
    ident: Option<c_long>,
    timeout_exc: Option<Py<PyAny>>,
    current: Option<Current>,
    next_seq: u64,
}

#[derive(Default)]
struct Worker {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    running: Mutex<Running>,
    /// Mirrors `Running::generation` for the idle wait.
    generation: AtomicU64,
    restarts: AtomicU64,
}

/// The shared worker and its supervisor, started on first use.
fn worker() -> &'static Worker {
    static WORKER: OnceLock<&'static Worker> = OnceLock::new();
    WORKER.get_or_init(|| {
        crate::ensure_initialized();
        let w: &'static Worker = Box::leak(Box::default());
        spawn_worker(w, 0);
        std::thread::Builder::new()
            .name("atd-dat-script-supervisor".into())
            .spawn(move || supervise(w))
            .expect("spawning a thread");
        w
    })
}

/// How often the Script worker was replaced after a run that did not stop
/// when interrupted.
pub fn restarts() -> u64 {
    worker().restarts.load(Ordering::Acquire)
}

fn spawn_worker(w: &'static Worker, generation: u64) {
    if let Err(e) = std::thread::Builder::new()
        .name(format!("atd-dat-script-{generation}"))
        .spawn(move || work(w, generation))
    {
        log::error!("cannot start the Script worker: {e}");
    }
}

/// A helper module of its own: each worker has a fresh namespace
/// (`PyModule::from_code` re-runs code in an existing module of the same
/// name, so each gets a unique one, taken out of `sys.modules` again).
fn helper(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!("_atd_dat_script_{}", NEXT.fetch_add(1, Ordering::AcqRel));
    let code = CString::new(HELPER).expect("the helper has no NUL");
    let cname = CString::new(name.as_str()).expect("module names have no NUL");
    let m = PyModule::from_code(py, &code, c"atd_dat_script.py", &cname)?;
    py.import("sys")?.getattr("modules")?.del_item(&name)?;
    Ok(m)
}

fn work(w: &'static Worker, generation: u64) {
    let helper: Result<Py<PyModule>, String> = Python::attach(|py| {
        let m = helper(py)?;
        let ident: u64 = m.getattr("thread_ident")?.call0()?.extract()?;
        let exc = m.getattr("ScriptTimeout")?.unbind();
        let mut r = lock(&w.running);
        if r.generation == generation {
            // Thread IDs are `unsigned long` in C; the FFI declares `long`.
            r.ident = Some(ident as c_ulong as c_long);
            r.timeout_exc = Some(exc);
        }
        Ok(m.unbind())
    })
    .map_err(|e: PyErr| format!("the Script helper did not load: {e}"));
    loop {
        let job = {
            let mut q = lock(&w.queue);
            loop {
                if w.generation.load(Ordering::Acquire) != generation {
                    return;
                }
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = w
                    .wake
                    .wait_timeout(q, IDLE_WAIT)
                    .map_or_else(|e| e.into_inner().0, |(g, _)| g);
            }
        };
        // Checked again here: the list may have changed since it was queued.
        let result = match &helper {
            _ if !crate::allow::allows_script(job.node, &job.code) => Err(not_allowed()),
            Ok(h) => match Python::attach(|py| supervised(py, w, generation, &job, h.bind(py))) {
                Some(r) => r,
                // Abandoned during the run: the supervisor answered the
                // node and a new worker took over the queue.
                None => return,
            },
            Err(e) => Err(e.clone()),
        };
        job.slot.answer(result);
    }
}

/// Runs `job` as the supervised current run. `None` if this worker was
/// abandoned meanwhile.
fn supervised(
    _gil: Python<'_>,
    w: &Worker,
    generation: u64,
    job: &Job,
    helper: &Bound<'_, PyModule>,
) -> Option<Result<Dat, String>> {
    {
        let mut r = lock(&w.running);
        if r.generation != generation {
            return None;
        }
        if let Some(timeout) = job.timeout {
            r.next_seq += 1;
            r.current = Some(Current {
                seq: r.next_seq,
                started: Instant::now(),
                timeout,
                interrupted: false,
                slot: job.slot.clone(),
            });
        }
    }
    let out = run(helper, job);
    let mut r = lock(&w.running);
    if r.generation != generation {
        return None;
    }
    if r.current.take().is_some_and(|c| c.interrupted)
        && let Some(ident) = r.ident
    {
        // The interrupt may not have fired inside the run (it ended just in
        // time): clear it so it does not hit the next one.
        // SAFETY: the GIL is held (`_gil`); a null exception clears the
        // thread's pending asynchronous exception.
        unsafe {
            pyo3::ffi::PyThreadState_SetAsyncExc(ident, std::ptr::null_mut());
        }
    }
    Some(out)
}

fn timed_out(timeout: Duration) -> String {
    format!(
        "TimeoutError: the script did not return within its timeout of {} s",
        timeout.as_secs_f32()
    )
}

fn supervise(w: &'static Worker) {
    loop {
        std::thread::sleep(SUPERVISE_EVERY);
        let mut r = lock(&w.running);
        let Some(c) = r.current.as_mut() else {
            continue;
        };
        let t = c.started.elapsed();
        if !c.interrupted && t > c.timeout {
            c.interrupted = true;
            let seq = c.seq;
            drop(r);
            interrupt(w, seq);
        } else if c.interrupted && t > c.timeout + GRACE {
            // Abandon the worker: the stuck thread writes nothing when (if)
            // its call returns, and a new one takes over the queue.
            let Some(c) = r.current.take() else { continue };
            r.generation += 1;
            r.ident = None;
            r.timeout_exc = None;
            let generation = r.generation;
            w.generation.store(generation, Ordering::Release);
            drop(r);
            w.restarts.fetch_add(1, Ordering::AcqRel);
            log::error!("a Script DAT did not stop after its timeout; replacing the Script worker");
            c.slot.answer(Err(format!(
                "{} and did not stop when interrupted (a blocking call?), so the Script worker \
                 was replaced",
                timed_out(c.timeout)
            )));
            spawn_worker(w, generation);
            w.wake.notify_all();
        }
    }
}

/// Raises `ScriptTimeout` in run `seq` if it is still the current one. On
/// a thread of its own: getting the GIL takes as long as a C call holds it.
fn interrupt(w: &'static Worker, seq: u64) {
    let spawned = std::thread::Builder::new()
        .name("atd-dat-script-interrupt".into())
        .spawn(move || {
            Python::attach(|py| {
                // The GIL first, then the lock: the worker takes the lock
                // only while it holds the GIL.
                let r = lock(&w.running);
                if r.current.as_ref().is_none_or(|c| c.seq != seq) {
                    return;
                }
                let (Some(ident), Some(exc)) =
                    (r.ident, r.timeout_exc.as_ref().map(|e| e.clone_ref(py)))
                else {
                    return;
                };
                // SAFETY: the GIL is held; `exc` is a live exception class,
                // and the call takes its own reference.
                unsafe {
                    pyo3::ffi::PyThreadState_SetAsyncExc(ident, exc.as_ptr());
                }
            });
        });
    if let Err(e) = spawned {
        log::error!("cannot interrupt a Script DAT: {e}");
    }
}

/// An input as the script sees it: `None`, a `str` or a list of rows.
fn to_py<'py>(py: Python<'py>, input: Option<&Dat>) -> PyResult<Bound<'py, PyAny>> {
    match input {
        None => Ok(py.None().into_bound(py)),
        Some(d) if d.is_text() => Ok(d.as_text().into_pyobject(py)?.into_any()),
        Some(d) => {
            let rows = PyList::empty(py);
            for row in d.as_table().iter_rows() {
                rows.append(PyList::new(py, row)?)?;
            }
            Ok(rows.into_any())
        }
    }
}

fn run(helper: &Bound<'_, PyModule>, job: &Job) -> Result<Dat, String> {
    let py = helper.py();
    // Errors in the script come back as ("error", message with its line);
    // these are conversions failing.
    let shown = |e: PyErr| e.to_string();
    let timeout = job.timeout.unwrap_or_default();
    let inputs = PyList::empty(py);
    for i in &job.inputs {
        inputs
            .append(to_py(py, i.as_ref()).map_err(shown)?)
            .map_err(shown)?;
    }
    let out = helper
        .getattr("run")
        .and_then(|f| f.call1((job.code.as_str(), inputs)))
        .map_err(|e| {
            // An interrupt that landed outside the script's own code.
            let e = e.to_string();
            if e.starts_with("ScriptTimeout") {
                timed_out(timeout)
            } else {
                e
            }
        })?;
    let (kind, value): (String, Bound<'_, PyAny>) = out.extract().map_err(shown)?;
    if kind == "timeout" {
        let line: u32 = value.extract().map_err(shown)?;
        let at = if line > 0 {
            format!(" (stopped at line {line})")
        } else {
            String::new()
        };
        return Err(format!("{}{at}", timed_out(timeout)));
    }
    if kind == "error" {
        return Err(value.extract::<String>().map_err(shown)?);
    }
    if kind == "text" {
        return Ok(Dat::text(value.extract::<String>().map_err(shown)?));
    }
    let rows: Vec<Vec<String>> = value.extract().map_err(shown)?;
    Ok(Dat::table(Table::from_rows(rows)))
}

/// One Script node.
#[derive(Debug, Default)]
struct ScriptOp {
    slot: Arc<Slot>,
    pending: bool,
    /// What the last job was made from.
    code: String,
    inputs: Vec<Option<Dat>>,
    generation: Option<u64>,
    /// The code is not on the allow-list (checked when the code, the inputs
    /// or the list change).
    blocked: bool,
    error: Option<String>,
    status: String,
}

fn not_allowed() -> String {
    format!(
        "{}: Script nodes run Python from the project once you allow it",
        crate::ops::NOT_ALLOWED
    )
}

impl ScriptOp {
    fn stale(&self, ctx: &DatCtx<'_>, generation: u64) -> bool {
        self.generation != Some(generation)
            || self.code != ctx.params.str("code")
            || self.inputs.len() != ctx.inputs.len()
            || self
                .inputs
                .iter()
                .enumerate()
                .any(|(i, old)| match (old, ctx.inputs.get(i)) {
                    (None, None) => false,
                    (Some(a), Some(b)) => !Dat::ptr_eq(a, b),
                    _ => true,
                })
    }

    fn take(&mut self, ctx: &mut DatCtx<'_>) {
        if let Some(r) = lock(&self.slot.result).take() {
            self.pending = false;
            match r {
                Ok(d) => {
                    self.status = if d.is_text() {
                        format!("text, {} bytes", d.as_text().len())
                    } else {
                        format!("table, {} × {}", d.rows(), d.cols())
                    };
                    ctx.output.replace(d);
                    self.error = None;
                }
                // The last good value stays.
                Err(e) => self.error = Some(e),
            }
        }
    }
}

impl DatOp for ScriptOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        if self.pending {
            self.take(ctx);
        }
        let generation = crate::allow::generation();
        if !self.pending && self.stale(ctx, generation) {
            self.generation = Some(generation);
            self.code.clear();
            self.code.push_str(ctx.params.str("code"));
            self.inputs.clear();
            self.inputs
                .extend((0..ctx.inputs.len()).map(|i| ctx.inputs.get(i).cloned()));
            self.blocked = !crate::allow::allows_script(ctx.params.node(), &self.code);
            if !self.blocked {
                let secs = f64::from(ctx.params.f32("timeout_s"));
                let timeout =
                    (secs > 0.0).then(|| Duration::from_secs_f64(secs.min(MAX_TIMEOUT_S)));
                let w = worker();
                lock(&w.queue).push_back(Job {
                    node: ctx.params.node(),
                    code: self.code.clone(),
                    inputs: self.inputs.clone(),
                    timeout,
                    slot: self.slot.clone(),
                });
                w.wake.notify_one();
                self.pending = true;
                if ctx.params.mode().is_offline() {
                    // Offline the frame waits for its script; with a
                    // timeout the supervisor answers within GRACE of it.
                    let wait =
                        timeout.map_or(OFFLINE_TIMEOUT, |t| t + GRACE + Duration::from_secs(1));
                    let deadline = Instant::now() + wait;
                    let mut r = lock(&self.slot.result);
                    while r.is_none() && Instant::now() < deadline {
                        r = self
                            .slot
                            .done
                            .wait_timeout(r, Duration::from_millis(100))
                            .map_or_else(|e| e.into_inner().0, |(g, _)| g);
                    }
                    let late = r.is_none();
                    drop(r);
                    if late {
                        return Err(format!(
                            "TimeoutError: the script did not return within {} s",
                            wait.as_secs_f64()
                        ));
                    }
                    self.take(ctx);
                }
            }
        }
        if self.blocked {
            return Err(not_allowed());
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

/// Creates a Script node.
pub fn create() -> Box<dyn DatOp> {
    Box::<ScriptOp>::default()
}

/// Adds the Script DAT's implementation to `library`.
pub fn library(library: &mut OpLibrary) {
    library.add_dat(TYPE_ID, create);
}
