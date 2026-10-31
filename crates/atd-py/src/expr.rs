//! Python parameter expressions (FR-P2, PRD A22): the Python tier.
//!
//! [`PyExprHost`] runs expressions outside the native subset on its own
//! thread. The cook thread hands over the frame's jobs and picks up
//! finished results without ever waiting: both sides use `try_lock` on
//! buffers that are swapped or reused, so nothing allocates on the cook
//! thread once warm. A result is used on the next frame (one frame late).
//! An offline render (FR-H1) waits for each frame's results instead
//! (`submit_wait`), so they are always exactly one frame late.
//!
//! Expressions are project code: one is evaluated only while its exact
//! content (node, parameter, source) is on the allow-list
//! ([`crate::allow`], `docs/python-ops.md`). The engine offers no other job
//! ([`PythonHost::allows`]), and the worker checks each job again before it
//! evaluates it.
//!
//! Expressions see the same names as the native tier (`time`, `frame`,
//! `me`, `op('name').par.x`, `op('name')['channel']`, the `math` module's
//! functions, `clamp` and `lerp`), plus the rest of Python.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use atd_core::expr::{ExprValue, PyJob, PythonHost};
use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Evaluates one job: compiles once per source, builds the namespace.
const HELPER: &std::ffi::CStr = cr#"
from math import *

def clamp(x, low, high):
    return max(low, min(high, x))

def lerp(a, b, t):
    return a + (b - a) * t

class _Par:
    __slots__ = ("_values",)

    def __init__(self, values):
        self._values = values

    def __getattr__(self, name):
        try:
            v = self._values[name]
        except KeyError:
            raise AttributeError(f"no number parameter '{name}'") from None
        return v[0] if len(v) == 1 else tuple(v)


class _Op:
    def __init__(self, name, params, channels):
        self.name = name
        self.par = _Par(params)
        self._channels = channels

    def __getitem__(self, channel):
        try:
            return self._channels[channel]
        except KeyError:
            raise KeyError(f"op('{self.name}') has no channel '{channel}'") from None


class _Me:
    def __init__(self, name):
        self.name = name
        digits = name[len(name.rstrip("0123456789")):]
        self.digits = int(digits) if digits else None


_base = {k: v for k, v in globals().items() if not k.startswith("_") or k == "__builtins__"}
_codes = {}


def evaluate(source, time, frame, me, refs):
    code = _codes.get(source)
    if code is None:
        code = compile(source, "<expression>", "eval")
        _codes[source] = code
    nodes = {}
    for name, exists, params, channels in refs:
        if exists:
            nodes[name] = _Op(name, dict(params), dict(channels))

    def op(name):
        try:
            return nodes[name]
        except KeyError:
            raise ValueError(f"op('{name}'): no such node") from None

    ns = dict(_base)
    ns.update(time=time, frame=int(frame), me=_Me(me), op=op)
    result = eval(code, ns)
    if isinstance(result, (tuple, list)):
        return tuple(float(v) for v in result)
    return float(result)
"#;

#[derive(Default)]
struct Inbox {
    jobs: Vec<PyJob>,
    fresh: bool,
    /// Batches offered so far; the worker reports the one it finished.
    batch: u64,
}

#[derive(Default)]
struct Shared {
    inbox: Mutex<Inbox>,
    wake: Condvar,
    results: Mutex<Vec<(u64, Result<ExprValue, String>)>>,
    stop: AtomicBool,
    /// The last batch whose results are in `results` (offline waits on it).
    finished: Mutex<u64>,
    done: Condvar,
}

/// How long an offline render waits for one frame's Python expressions
/// before they become node errors (the render report listed an unbounded
/// wait as a hang risk).
pub const OFFLINE_TIMEOUT: Duration = Duration::from_secs(10);

/// The Python worker for expressions.
pub struct PyExprHost {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    offline_timeout: Duration,
}

impl std::fmt::Debug for PyExprHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyExprHost").finish_non_exhaustive()
    }
}

impl PyExprHost {
    /// Starts the worker (and the embedded interpreter, once per process).
    pub fn start() -> std::io::Result<Self> {
        crate::ensure_initialized();
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("atd-python-expr".into())
            .spawn(move || run(&worker))?;
        Ok(Self {
            shared,
            thread: Some(thread),
            offline_timeout: OFFLINE_TIMEOUT,
        })
    }

    /// Changes how long [`PythonHost::submit_wait`] waits (tests).
    pub fn set_offline_timeout(&mut self, timeout: Duration) {
        self.offline_timeout = timeout;
    }
}

impl Drop for PyExprHost {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        // Not joined: an expression stuck in Python must not stall whoever
        // drops the host; the thread ends when its evaluation returns.
        drop(self.thread.take());
    }
}

impl PythonHost for PyExprHost {
    fn generation(&self) -> u64 {
        crate::allow::generation()
    }

    fn allows(&self, node: atd_core::graph::NodeId, param: &str, source: &str) -> bool {
        crate::allow::allows_expr(node, param, source)
    }

    fn submit(&mut self, jobs: &[PyJob]) {
        // The worker holds the lock only to swap buffers; if it has it now,
        // this frame's jobs are skipped.
        let Ok(mut inbox) = self.shared.inbox.try_lock() else {
            return;
        };
        inbox.jobs.truncate(jobs.len());
        let common = inbox.jobs.len();
        for (dst, src) in inbox.jobs.iter_mut().zip(jobs) {
            dst.clone_from(src);
        }
        inbox.jobs.extend_from_slice(&jobs[common..]);
        inbox.fresh = true;
        inbox.batch += 1;
        drop(inbox);
        self.shared.wake.notify_one();
    }

    fn submit_wait(&mut self, jobs: &[PyJob]) {
        let batch = {
            let mut inbox = self.shared.inbox.lock().unwrap_or_else(|e| e.into_inner());
            inbox.jobs.clear();
            inbox.jobs.extend_from_slice(jobs);
            inbox.fresh = true;
            inbox.batch += 1;
            inbox.batch
        };
        self.shared.wake.notify_one();
        let deadline = Instant::now() + self.offline_timeout;
        let mut finished = self
            .shared
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while *finished < batch && self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            finished = self
                .shared
                .done
                .wait_timeout(finished, left.min(Duration::from_millis(100)))
                .map_or_else(|e| e.into_inner().0, |(g, _)| g);
        }
        if *finished >= batch {
            return;
        }
        drop(finished);
        // Late: the frame's expressions fail on their nodes, so the render
        // stops with a message instead of hanging.
        let message = if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            format!(
                "TimeoutError: the Python expression worker did not finish within {} s",
                self.offline_timeout.as_secs_f64()
            )
        } else {
            "the Python expression worker stopped".to_owned()
        };
        let mut results = self
            .shared
            .results
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        results.retain(|(k, _)| !jobs.iter().any(|j| j.key == *k));
        results.extend(jobs.iter().map(|j| (j.key, Err(message.clone()))));
    }

    fn collect(&mut self, out: &mut Vec<(u64, Result<ExprValue, String>)>) {
        out.clear();
        if let Ok(mut results) = self.shared.results.try_lock() {
            std::mem::swap(out, &mut *results);
        }
    }
}

fn run(shared: &Shared) {
    let mut jobs: Vec<PyJob> = Vec::new();
    let mut done: Vec<(u64, Result<ExprValue, String>)> = Vec::new();
    let helper: Option<Py<PyModule>> = Python::attach(|py| {
        PyModule::from_code(py, HELPER, c"atd_expr.py", c"atd_expr")
            .map(Bound::unbind)
            .map_err(|e| log::error!("Python expression helper: {e}"))
            .ok()
    });
    loop {
        let batch = {
            let mut inbox = shared.inbox.lock().unwrap_or_else(|e| e.into_inner());
            while !inbox.fresh && !shared.stop.load(Ordering::Relaxed) {
                inbox = shared.wake.wait(inbox).unwrap_or_else(|e| e.into_inner());
            }
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            std::mem::swap(&mut jobs, &mut inbox.jobs);
            inbox.fresh = false;
            inbox.batch
        };
        Python::attach(|py| {
            for job in &jobs {
                // Checked again here: the list may have changed (another
                // project opened) since the job was offered.
                let result = if !crate::allow::allows_expr(job.node, &job.param, &job.source) {
                    Err(atd_core::expr::PYTHON_NOT_ALLOWED.to_owned())
                } else {
                    match &helper {
                        Some(h) => evaluate(h.bind(py), job),
                        None => Err("the Python expression helper did not load".into()),
                    }
                };
                done.push((job.key, result));
            }
        });
        let mut results = shared.results.lock().unwrap_or_else(|e| e.into_inner());
        // Keep only the newest result per key if the engine has not
        // collected the previous batch.
        results.retain(|(k, _)| !done.iter().any(|(d, _)| d == k));
        results.append(&mut done);
        drop(results);
        *shared.finished.lock().unwrap_or_else(|e| e.into_inner()) = batch;
        shared.done.notify_all();
    }
}

/// A referenced node as the helper takes it: (name, exists, parameters,
/// channels).
type RefTuple<'a> = (&'a str, bool, Vec<(&'a str, Vec<f64>)>, Vec<(&'a str, f64)>);

fn evaluate(helper: &Bound<'_, PyModule>, job: &PyJob) -> Result<ExprValue, String> {
    let refs: Vec<RefTuple<'_>> = job
        .refs
        .iter()
        .map(|r| {
            (
                &*r.name,
                r.exists,
                r.params.iter().map(|(n, v)| (&**n, v.clone())).collect(),
                r.channels.iter().map(|(n, v)| (&**n, *v)).collect(),
            )
        })
        .collect();
    let result = helper
        .getattr("evaluate")
        .and_then(|f| f.call1((&*job.source, job.time, job.frame, &*job.me, refs)))
        // e.g. "ZeroDivisionError: division by zero"
        .map_err(|e| e.to_string())?;
    // The helper gives a float, or a tuple of floats for a vector.
    if let Ok(v) = result.extract::<f64>() {
        return Ok(ExprValue::Number(v));
    }
    let values = result.extract::<Vec<f64>>().map_err(|e| e.to_string())?;
    ExprValue::tuple(&values).ok_or_else(|| {
        format!(
            "gives {} numbers; a vector or colour takes one to four",
            values.len()
        )
    })
}
