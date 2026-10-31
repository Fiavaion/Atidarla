//! Script DAT (`dat.script`): a `cook(inputs)` function in Python that
//! turns DAT inputs into a DAT value. The schema is registered with the
//! DAT family (`atd-ops-dat`); this is its implementation, added to the
//! engine's library with [`library`].
//!
//! Scripts run on one worker thread shared by every Script node, never on
//! the cook thread (CLAUDE.md rule 1): a node hands its code and inputs
//! over when either changes and picks the result up on a later frame (an
//! offline render waits for it). A script is project code, so it runs only
//! once the project's Python is allowed ([`allow`], set together with the
//! project's Python operators: `docs/python-ops.md`); until then the node
//! reports [`NOT_ALLOWED`](crate::ops::NOT_ALLOWED).
//!
//! **Known limit:** there is no timeout yet. A script that never returns
//! holds the worker: other Script nodes stop updating (the show does not).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use atd_gpu::engine::{Dat, DatCtx, DatOp, OpLibrary, Table};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyModule};

/// The operator type this implements.
pub const TYPE_ID: &str = "dat.script";

/// How long an offline render waits for one script.
pub const OFFLINE_TIMEOUT: Duration = Duration::from_secs(30);

const HELPER: &std::ffi::CStr = cr#"
_fns = {}

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

static ALLOWED: AtomicBool = AtomicBool::new(false);
/// Bumped by every [`allow`] change, so nodes run again when it flips.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Lets the open project's network Python run (`true`) or stops it
/// (`false`): Script nodes and Python-tier parameter expressions
/// ([`crate::expr::PyExprHost`]). Process-wide, like Sovereign mode: one
/// project is open at a time. Set with the project's Python operators
/// ([`crate::ops::PythonOps::allow`]), but only on an explicit choice:
/// the remembered `ops/` hash does not cover network code.
pub fn allow(on: bool) {
    if ALLOWED.swap(on, Ordering::AcqRel) != on {
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

pub fn allowed() -> bool {
    ALLOWED.load(Ordering::Acquire)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where one node's result arrives.
#[derive(Debug, Default)]
struct Slot {
    result: Mutex<Option<Result<Dat, String>>>,
    done: Condvar,
}

struct Job {
    code: String,
    inputs: Vec<Option<Dat>>,
    slot: Arc<Slot>,
}

#[derive(Default)]
struct Worker {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
}

/// The shared worker, started on first use.
fn worker() -> &'static Worker {
    static WORKER: OnceLock<&'static Worker> = OnceLock::new();
    WORKER.get_or_init(|| {
        crate::ensure_initialized();
        let w: &'static Worker = Box::leak(Box::default());
        std::thread::Builder::new()
            .name("atd-dat-script".into())
            .spawn(move || work(w))
            .expect("spawning a thread");
        w
    })
}

fn work(w: &Worker) {
    let helper: Result<Py<PyModule>, String> = Python::attach(|py| {
        PyModule::from_code(py, HELPER, c"atd_dat_script.py", c"atd_dat_script")
            .map(Bound::unbind)
            .map_err(|e| format!("the Script helper did not load: {e}"))
    });
    loop {
        let job = {
            let mut q = lock(&w.queue);
            loop {
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = w.wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = match &helper {
            Ok(h) => Python::attach(|py| run(h.bind(py), &job)),
            Err(e) => Err(e.clone()),
        };
        *lock(&job.slot.result) = Some(result);
        job.slot.done.notify_all();
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
    let inputs = PyList::empty(py);
    for i in &job.inputs {
        inputs
            .append(to_py(py, i.as_ref()).map_err(shown)?)
            .map_err(shown)?;
    }
    let out = helper
        .getattr("run")
        .and_then(|f| f.call1((job.code.as_str(), inputs)))
        .map_err(shown)?;
    let (kind, value): (String, Bound<'_, PyAny>) = out.extract().map_err(shown)?;
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
    error: Option<String>,
    status: String,
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
                Err(e) => self.error = Some(e),
            }
        }
    }
}

impl DatOp for ScriptOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        if !allowed() {
            self.generation = None;
            return Err(format!(
                "{}: Script nodes run Python from the project once you allow it",
                crate::ops::NOT_ALLOWED
            ));
        }
        if self.pending {
            self.take(ctx);
        }
        let generation = GENERATION.load(Ordering::Acquire);
        if !self.pending && self.stale(ctx, generation) {
            self.generation = Some(generation);
            self.code.clear();
            self.code.push_str(ctx.params.str("code"));
            self.inputs.clear();
            self.inputs
                .extend((0..ctx.inputs.len()).map(|i| ctx.inputs.get(i).cloned()));
            let w = worker();
            lock(&w.queue).push_back(Job {
                code: self.code.clone(),
                inputs: self.inputs.clone(),
                slot: self.slot.clone(),
            });
            w.wake.notify_one();
            self.pending = true;
            if ctx.params.mode().is_offline() {
                // Offline the frame waits for its script.
                let deadline = Instant::now() + OFFLINE_TIMEOUT;
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
                        OFFLINE_TIMEOUT.as_secs()
                    ));
                }
                self.take(ctx);
            }
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
