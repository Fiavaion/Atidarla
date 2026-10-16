//! Python parameter expressions (FR-P2, PRD A22): the Python tier.
//!
//! [`PyExprHost`] runs expressions outside the native subset on its own
//! thread. The cook thread hands over the frame's jobs and picks up
//! finished results without ever waiting: both sides use `try_lock` on
//! buffers that are swapped or reused, so nothing allocates on the cook
//! thread once warm. A result is used on the next frame (one frame late).
//!
//! Expressions see the same names as the native tier (`time`, `frame`,
//! `me`, `op('name').par.x`, `op('name')['channel']`, the `math` module's
//! functions, `clamp` and `lerp`), plus the rest of Python.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use atd_core::expr::{PyJob, PythonHost};
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
    return float(eval(code, ns))
"#;

#[derive(Default)]
struct Inbox {
    jobs: Vec<PyJob>,
    fresh: bool,
}

#[derive(Default)]
struct Shared {
    inbox: Mutex<Inbox>,
    wake: Condvar,
    results: Mutex<Vec<(u64, Result<f64, String>)>>,
    stop: AtomicBool,
}

/// The Python worker for expressions.
pub struct PyExprHost {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
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
        })
    }
}

impl Drop for PyExprHost {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl PythonHost for PyExprHost {
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
        drop(inbox);
        self.shared.wake.notify_one();
    }

    fn collect(&mut self, out: &mut Vec<(u64, Result<f64, String>)>) {
        out.clear();
        if let Ok(mut results) = self.shared.results.try_lock() {
            std::mem::swap(out, &mut *results);
        }
    }
}

fn run(shared: &Shared) {
    let mut jobs: Vec<PyJob> = Vec::new();
    let mut done: Vec<(u64, Result<f64, String>)> = Vec::new();
    let helper: Option<Py<PyModule>> = Python::attach(|py| {
        PyModule::from_code(py, HELPER, c"atd_expr.py", c"atd_expr")
            .map(Bound::unbind)
            .map_err(|e| log::error!("Python expression helper: {e}"))
            .ok()
    });
    loop {
        {
            let mut inbox = shared.inbox.lock().unwrap_or_else(|e| e.into_inner());
            while !inbox.fresh && !shared.stop.load(Ordering::Relaxed) {
                inbox = shared.wake.wait(inbox).unwrap_or_else(|e| e.into_inner());
            }
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            std::mem::swap(&mut jobs, &mut inbox.jobs);
            inbox.fresh = false;
        }
        Python::attach(|py| {
            for job in &jobs {
                let result = match &helper {
                    Some(h) => evaluate(h.bind(py), job),
                    None => Err("the Python expression helper did not load".into()),
                };
                done.push((job.key, result));
            }
        });
        let mut results = shared.results.lock().unwrap_or_else(|e| e.into_inner());
        // Keep only the newest result per key if the engine has not
        // collected the previous batch.
        results.retain(|(k, _)| !done.iter().any(|(d, _)| d == k));
        results.append(&mut done);
    }
}

/// A referenced node as the helper takes it: (name, exists, parameters,
/// channels).
type RefTuple<'a> = (&'a str, bool, Vec<(&'a str, Vec<f64>)>, Vec<(&'a str, f64)>);

fn evaluate(helper: &Bound<'_, PyModule>, job: &PyJob) -> Result<f64, String> {
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
    helper
        .getattr("evaluate")
        .and_then(|f| f.call1((&*job.source, job.time, job.frame, &*job.me, refs)))
        .and_then(|v| v.extract::<f64>())
        // e.g. "ZeroDivisionError: division by zero"
        .map_err(|e| e.to_string())
}
