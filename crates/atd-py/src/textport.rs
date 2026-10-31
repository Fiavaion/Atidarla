//! The textport: an interactive Python console over the open project
//! (FR-P5, `docs/textport.md`).
//!
//! [`Textport`] runs what the user types on its own worker thread, never on
//! the UI or render thread (CLAUDE.md rule 1): [`Textport::submit`] only
//! queues the text, and output comes back through [`Textport::poll`] as it is
//! written. The worker is separate from the expression and Python operator
//! workers, so a long statement in the console never delays an expression or
//! a cook; it shares their embedded interpreter.
//!
//! The console's `atidarla` API reads a snapshot of the project that the app
//! publishes ([`Textport::publish`], [`Textport::sync`]) and sends parameter
//! changes back; [`Textport::sync`] applies them on the app's thread as
//! undoable edits ([`Engine::edit`]), so the worker never touches the graph.
//!
//! **Trust.** The textport runs only what the user types, so it needs no
//! trust gate of its own. It never loads a project's `ops/` files, and while
//! the project's Python operators are not allowed it refuses to import from
//! its `ops/` folder ([`Textport::set_project`]).

use std::ffi::CString;
use std::os::raw::c_long;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atd_core::graph::{Edit, Param};
use atd_gpu::engine::Engine;
use pyo3::exceptions::{PyKeyboardInterrupt, PyTimeoutError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyModule;
use serde_json::Value;

/// The console's Python side.
const HELPER: &str = include_str!("textport_host.py");

/// How long `atidarla.set_par` waits for the app to apply the edit.
pub const SET_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a piece of output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// `print` and other writes to `sys.stdout`.
    Out,
    /// Tracebacks, syntax errors and writes to `sys.stderr`.
    Err,
    /// The `repr` of an expression's value.
    Result,
}

/// What the worker reports, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Output {
        stream: Stream,
        text: String,
    },
    /// An entry finished; `more` when the console waits for more lines.
    Done {
        more: bool,
    },
    /// The namespace was replaced ([`Textport::set_project`], [`Textport::reset`]).
    Reset,
}

/// A parameter change from the console, for the app's thread.
#[derive(Debug)]
struct SetRequest {
    node: String,
    param: String,
    value: Value,
    reply: Sender<Result<(), String>>,
}

#[derive(Debug)]
enum Request {
    Run {
        id: u64,
        text: String,
    },
    Reset {
        ops_dir: Option<PathBuf>,
        allowed: bool,
    },
}

#[derive(Debug, Default)]
struct Shared {
    /// The project as last published: (version, JSON).
    snapshot: Mutex<(u64, Arc<str>)>,
    frame: AtomicU64,
    /// `f64` bits.
    time: AtomicU64,
    /// The entry running now (0 when idle); changed only with the GIL held.
    running: AtomicU64,
    /// The worker's Python thread ID (0 before it started).
    ident: AtomicU64,
}

/// The Rust side of a console, handed to Python.
#[pyclass(frozen)]
struct Bridge {
    shared: Arc<Shared>,
    events: Sender<Event>,
    sets: Sender<SetRequest>,
}

#[pymethods]
impl Bridge {
    fn write(&self, stream: &str, text: String) {
        let stream = match stream {
            "err" => Stream::Err,
            "result" => Stream::Result,
            _ => Stream::Out,
        };
        let _ = self.events.send(Event::Output { stream, text });
    }

    fn snapshot(&self) -> (u64, String) {
        let s = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (s.0, s.1.to_string())
    }

    fn frame(&self) -> u64 {
        self.shared.frame.load(Ordering::Acquire)
    }

    fn time(&self) -> f64 {
        f64::from_bits(self.shared.time.load(Ordering::Acquire))
    }

    /// Queues the edit for the app and waits (without the GIL) until it is
    /// applied.
    fn set_par(&self, py: Python<'_>, node: String, param: String, value: &str) -> PyResult<()> {
        let value: Value =
            serde_json::from_str(value).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let (reply, answer) = channel();
        self.sets
            .send(SetRequest {
                node,
                param,
                value,
                reply,
            })
            .map_err(|_| PyValueError::new_err("the textport is closing"))?;
        match py.detach(move || answer.recv_timeout(SET_TIMEOUT)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(PyValueError::new_err(e)),
            Err(RecvTimeoutError::Timeout) => Err(PyTimeoutError::new_err(format!(
                "the app did not apply the edit within {} s; it may still apply it",
                SET_TIMEOUT.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(PyValueError::new_err("the textport is closing"))
            }
        }
    }
}

/// The helper module, loaded once per process.
fn helper(py: Python<'_>) -> PyResult<&'static Py<PyModule>> {
    static HELPER_MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();
    HELPER_MODULE.get_or_try_init(py, || {
        let code = CString::new(HELPER).expect("the helper has no NUL");
        let module = PyModule::from_code(py, &code, c"atd_textport.py", c"_atd_textport")?;
        module
            .getattr("install")?
            .call1((py.import("atidarla")?,))?;
        Ok(module.unbind())
    })
}

/// An interactive Python console over the open project. Dropping it stops
/// the worker once its current entry returns (it is not joined: a stuck
/// statement must not stall the app).
#[derive(Debug)]
pub struct Textport {
    shared: Arc<Shared>,
    requests: Sender<Request>,
    events: Receiver<Event>,
    sets: Receiver<SetRequest>,
    next_id: u64,
    /// Entries submitted and not yet done.
    pending: usize,
    more: bool,
    /// The project folder and whether its Python operators may run.
    project: (Option<PathBuf>, bool),
}

impl Textport {
    /// Starts the worker (and the embedded interpreter, once per process)
    /// with a fresh namespace and no project.
    pub fn start() -> std::io::Result<Self> {
        crate::ensure_initialized();
        let shared = Arc::new(Shared::default());
        *shared.snapshot.lock().unwrap_or_else(|e| e.into_inner()) =
            (0, Arc::from(r#"{"nodes":[]}"#));
        let (requests, inbox) = channel();
        let (events_tx, events) = channel();
        let (sets_tx, sets) = channel();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("atd-python-textport".into())
            .spawn(move || run(worker, &inbox, &events_tx, sets_tx))?;
        Ok(Self {
            shared,
            requests,
            events,
            sets,
            next_id: 0,
            pending: 0,
            more: false,
            project: (None, false),
        })
    }

    /// Queues `text` (one or more lines) to run; returns at once. Publish
    /// the project first ([`Self::publish`]) so the entry reads it as it is.
    pub fn submit(&mut self, text: &str) {
        self.next_id += 1;
        let text = text.replace("\r\n", "\n");
        if self
            .requests
            .send(Request::Run {
                id: self.next_id,
                text,
            })
            .is_ok()
        {
            self.pending += 1;
        }
    }

    /// An entry is queued or running.
    pub fn is_busy(&self) -> bool {
        self.pending > 0
    }

    /// The last entry was an incomplete statement: the next one continues it.
    pub fn wants_more(&self) -> bool {
        self.more
    }

    /// Moves what the worker reported since the last call into `out`.
    pub fn poll(&mut self, out: &mut Vec<Event>) {
        loop {
            match self.events.try_recv() {
                Ok(e) => {
                    match &e {
                        Event::Done { more } => {
                            self.pending = self.pending.saturating_sub(1);
                            self.more = *more;
                        }
                        Event::Reset => self.more = false,
                        Event::Output { .. } => {}
                    }
                    out.push(e);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.pending = 0;
                    return;
                }
            }
        }
    }

    /// A project was opened (or a new one started): the console starts
    /// again with a fresh namespace. `ops_dir` is the project's `ops/`
    /// folder; unless `allowed`, the console refuses to import from it.
    pub fn set_project(&mut self, ops_dir: Option<PathBuf>, allowed: bool) {
        self.project = (ops_dir, allowed);
        self.reset();
    }

    /// The project's Python operators were allowed: imports from its
    /// `ops/` folder work from the next fresh namespace on, and the current
    /// one is replaced now.
    pub fn allow_ops(&mut self) {
        if !self.project.1 {
            self.project.1 = true;
            self.reset();
        }
    }

    /// Replaces the namespace (stopping the running entry first).
    pub fn reset(&mut self) {
        self.interrupt();
        let (ops_dir, allowed) = self.project.clone();
        let _ = self.requests.send(Request::Reset { ops_dir, allowed });
        self.more = false;
    }

    /// Raises `KeyboardInterrupt` in the running entry. It takes effect when
    /// the entry next runs Python code (a `time.sleep` finishes first).
    /// Never waits for the interpreter: a helper thread does the work.
    pub fn interrupt(&self) {
        let id = self.shared.running.load(Ordering::Acquire);
        if id == 0 {
            return;
        }
        let shared = self.shared.clone();
        let spawned = std::thread::Builder::new()
            .name("atd-python-textport-interrupt".into())
            .spawn(move || {
                Python::attach(|py| {
                    // With the GIL held the worker cannot finish the entry
                    // and start another under us.
                    if shared.running.load(Ordering::Acquire) != id {
                        return;
                    }
                    let ident = shared.ident.load(Ordering::Acquire);
                    let exc = py.get_type::<PyKeyboardInterrupt>();
                    // SAFETY: the GIL is held; `exc` is a live exception
                    // class and the call takes its own reference. Thread IDs
                    // are `unsigned long` in C; the FFI declares `long`.
                    unsafe {
                        pyo3::ffi::PyThreadState_SetAsyncExc(
                            ident as std::os::raw::c_ulong as c_long,
                            exc.as_ptr(),
                        );
                    }
                });
            });
        if let Err(e) = spawned {
            log::warn!("textport: cannot interrupt: {e}");
        }
    }

    /// Publishes the frame and time the app cooked last.
    pub fn set_clock(&self, frame: u64, time: f64) {
        self.shared.frame.store(frame, Ordering::Release);
        self.shared.time.store(time.to_bits(), Ordering::Release);
    }

    /// Publishes the project as the console's `atidarla` API sees it:
    /// every node with its type and parameter values (the value an
    /// expression produced last, for a parameter that follows one; a
    /// vector or colour as its node read it, with bindings and expressions).
    pub fn publish(&self, engine: &Engine) {
        let reg = engine.registry();
        let net = engine.network();
        let nodes: Vec<Value> = net
            .nodes()
            .map(|n| {
                let params: Vec<Value> = reg
                    .get(&n.type_id)
                    .map(|s| s.params.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|p| {
                        let stored = || {
                            net.param_value(reg, n.id, &p.name)
                                .cloned()
                                .unwrap_or(Value::Null)
                        };
                        let value = match p.kind.components() {
                            // A vector or colour as driven (A66).
                            Some(_) => match stored() {
                                Value::Array(a) => (0..a.len())
                                    .map(|i| {
                                        engine
                                            .param_component(n.id, &p.name, i, true)
                                            .map_or(Value::Null, Value::from)
                                    })
                                    .collect(),
                                other => other,
                            },
                            None => engine
                                .expression_value(n.id, &p.name)
                                .and_then(atd_core::expr::ExprValue::number)
                                .map_or_else(stored, Value::from),
                        };
                        Value::Array(vec![Value::from(p.name.clone()), value])
                    })
                    .collect();
                serde_json::json!({ "name": n.name, "type": n.type_id, "params": params })
            })
            .collect();
        let text = serde_json::json!({ "nodes": nodes }).to_string();
        let mut s = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *s.1 != *text {
            *s = (s.0 + 1, Arc::from(text));
        }
    }

    /// Once per app frame, on the app's thread: publishes the clock,
    /// applies parameter changes the console asked for (each one undoable
    /// edit, labelled `Textport: node.param`) and, while an entry runs,
    /// publishes the project. Returns how many edits were applied.
    pub fn sync(&mut self, engine: &mut Engine, time: f64) -> usize {
        self.set_clock(engine.frame(), time);
        let mut answers = Vec::new();
        while let Ok(req) = self.sets.try_recv() {
            let result = apply_set(engine, &req);
            answers.push((req.reply, result));
        }
        if !answers.is_empty() || self.is_busy() {
            self.publish(engine);
        }
        let applied = answers.iter().filter(|(_, r)| r.is_ok()).count();
        // Answer after publishing, so the console reads the new values.
        for (reply, result) in answers {
            let _ = reply.send(result);
        }
        applied
    }
}

/// One console parameter change as an undoable edit.
fn apply_set(engine: &mut Engine, req: &SetRequest) -> Result<(), String> {
    let node = engine
        .network()
        .node_by_name(&req.node)
        .ok_or_else(|| format!("no node named '{}'", req.node))?;
    let id = node.id;
    let known = engine
        .registry()
        .get(&node.type_id)
        .is_some_and(|s| s.params.iter().any(|p| p.name == req.param));
    if !known {
        return Err(format!(
            "node '{}' has no parameter '{}'",
            req.node, req.param
        ));
    }
    let label = format!("Textport: {}.{}", req.node, req.param);
    engine
        .edit(
            &label,
            [Edit::SetParam {
                node: id,
                param: req.param.clone(),
                value: Some(Param::constant(req.value.clone())),
            }],
        )
        .map_err(|e| e.to_string())
}

fn run(
    shared: Arc<Shared>,
    inbox: &Receiver<Request>,
    events: &Sender<Event>,
    sets: Sender<SetRequest>,
) {
    let setup = Python::attach(|py| -> PyResult<(Py<Bridge>, Py<PyAny>)> {
        let helper = helper(py)?.bind(py);
        let ident: u64 = helper.getattr("thread_ident")?.call0()?.extract()?;
        shared.ident.store(ident, Ordering::Release);
        let bridge = Py::new(
            py,
            Bridge {
                shared: shared.clone(),
                events: events.clone(),
                sets,
            },
        )?;
        let console = new_console(py, &bridge, None, false)?;
        Ok((bridge, console))
    });
    let (bridge, mut console) = match setup {
        Ok(s) => s,
        Err(e) => {
            log::error!("textport: {e}");
            let _ = events.send(Event::Output {
                stream: Stream::Err,
                text: format!("The Python console could not start: {e}\n"),
            });
            // Answer every entry so the panel does not wait for ever.
            while let Ok(req) = inbox.recv() {
                if let Request::Run { .. } = req {
                    let _ = events.send(Event::Done { more: false });
                }
            }
            return;
        }
    };
    while let Ok(req) = inbox.recv() {
        match req {
            Request::Run { id, text } => {
                let more = Python::attach(|py| {
                    shared.running.store(id, Ordering::Release);
                    let result = console
                        .bind(py)
                        .call_method1("push", (text,))
                        .and_then(|v| v.extract::<bool>());
                    let ident = shared.ident.load(Ordering::Acquire);
                    // An interrupt that arrived as the entry ended must not
                    // hit the next one.
                    // SAFETY: the GIL is held; a null exception clears the
                    // thread's pending asynchronous exception.
                    unsafe {
                        pyo3::ffi::PyThreadState_SetAsyncExc(
                            ident as std::os::raw::c_ulong as c_long,
                            std::ptr::null_mut(),
                        );
                    }
                    shared.running.store(0, Ordering::Release);
                    result.unwrap_or_else(|e| {
                        let _ = events.send(Event::Output {
                            stream: Stream::Err,
                            text: format!("{e}\n"),
                        });
                        false
                    })
                });
                let _ = events.send(Event::Done { more });
            }
            Request::Reset { ops_dir, allowed } => {
                let fresh = Python::attach(|py| new_console(py, &bridge, ops_dir, allowed));
                match fresh {
                    Ok(c) => console = c,
                    Err(e) => log::error!("textport: {e}"),
                }
                let _ = events.send(Event::Reset);
            }
        }
    }
    Python::attach(|py| {
        if let Ok(h) = helper(py) {
            let _ = h.bind(py).getattr("leave").and_then(|f| f.call0());
        }
    });
}

fn new_console(
    py: Python<'_>,
    bridge: &Py<Bridge>,
    ops_dir: Option<PathBuf>,
    allowed: bool,
) -> PyResult<Py<PyAny>> {
    let helper = helper(py)?.bind(py);
    let dir = ops_dir.map(|d| d.to_string_lossy().into_owned());
    Ok(helper
        .getattr("new_console")?
        .call1((bridge.clone_ref(py), dir, allowed))?
        .unbind())
}
