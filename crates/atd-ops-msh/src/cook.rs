//! How an MSH node cooks (design `docs/design/msh-operators.md` M2-M8): the
//! render thread snapshots parameters and input handles, a worker thread
//! runs the operator and uploads the result, the render thread swaps it in.
//! Live, nothing waits (rule 1); offline (`CookMode::Offline`), the cook
//! waits so frame `i` uses the inputs of frame `i`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use atd_core::geo::{AttrClass, AttrType};
use atd_core::registry::{OperatorSchema, ParamKind};
use atd_gpu::engine::{CookMode, GeoCtx, GeoOp, Params};
use atd_gpu::geo::readback::{Mapped, Readback};
use atd_gpu::geo::{Attribute, BufferPool, Geometry};
use serde_json::Value;

use crate::mesh::Mesh;

/// The work of one operator: a pure function of its parameters and input
/// meshes, run on a worker thread.
pub type RunFn = fn(&Snap, &[Option<Mesh>], &Job) -> Result<Mesh, String>;

/// The error a cancelled job returns (never shown).
pub const CANCELLED: &str = "cancelled";

/// A running job's handle: operators call [`Job::check`] in long loops so
/// a superseded cook stops early.
#[derive(Debug, Clone, Default)]
pub struct Job {
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// A job nobody cancels (tests and tools).
    pub fn new() -> Self {
        Self::default()
    }

    /// `Err` once the job is cancelled.
    pub fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }

    /// [`Self::check`] every 4096th `i`.
    pub fn every(&self, i: usize) -> Result<(), String> {
        if i & 4095 == 0 { self.check() } else { Ok(()) }
    }
}

/// A snapshot value of one parameter.
#[derive(Debug, Clone)]
pub enum Val {
    Num(f64),
    Bool(bool),
    Str(String),
    Vec([f32; 4]),
}

impl PartialEq for Val {
    fn eq(&self, other: &Val) -> bool {
        match (self, other) {
            (Val::Num(a), Val::Num(b)) => a.to_bits() == b.to_bits(),
            (Val::Bool(a), Val::Bool(b)) => a == b,
            (Val::Str(a), Val::Str(b)) => a == b,
            (Val::Vec(a), Val::Vec(b)) => a.map(f32::to_bits) == b.map(f32::to_bits),
            _ => false,
        }
    }
}

/// An owned snapshot of a node's parameters, taken on the render thread
/// and read on the worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Snap {
    names: Arc<[String]>,
    vals: Vec<Val>,
}

impl Snap {
    /// The schema defaults with `overrides` (tests and tools).
    pub fn from_values(schema: &OperatorSchema, overrides: &[(&str, Value)]) -> Self {
        let names: Arc<[String]> = schema.params.iter().map(|p| p.name.clone()).collect();
        let vals = schema
            .params
            .iter()
            .map(|p| {
                let v = overrides
                    .iter()
                    .find(|(n, _)| *n == p.name)
                    .map_or(&p.default, |(_, v)| v);
                from_json(&p.kind, v)
            })
            .collect();
        Self { names, vals }
    }

    fn empty(schema: &OperatorSchema) -> Self {
        Self::from_values(schema, &[])
    }

    /// Overwrites the values from the engine's parameters, reusing storage
    /// (no allocation when string values keep their length class).
    fn fill(&mut self, kinds: &[ParamKind], p: &Params<'_>) {
        for ((name, kind), val) in self.names.iter().zip(kinds).zip(&mut self.vals) {
            match kind {
                ParamKind::Float { .. } => *val = Val::Num(f64::from(p.f32(name))),
                ParamKind::Int { .. } => *val = Val::Num(p.i64(name) as f64),
                ParamKind::Toggle | ParamKind::Pulse => *val = Val::Bool(p.bool(name)),
                ParamKind::Colour | ParamKind::Vec { .. } => *val = Val::Vec(p.vec4(name)),
                ParamKind::String | ParamKind::Menu { .. } => set_str(val, p.str(name)),
                ParamKind::Path => {
                    let s = p.str(name);
                    if s.is_empty() {
                        set_str(val, "");
                    } else {
                        set_str(val, &p.resolve(s).to_string_lossy());
                    }
                }
            }
        }
    }

    fn get(&self, name: &str) -> Option<&Val> {
        let i = self.names.iter().position(|n| n == name)?;
        self.vals.get(i)
    }

    pub fn f64(&self, name: &str) -> f64 {
        match self.get(name) {
            Some(Val::Num(v)) => *v,
            Some(Val::Bool(b)) => f64::from(u8::from(*b)),
            _ => 0.0,
        }
    }

    pub fn f32(&self, name: &str) -> f32 {
        self.f64(name) as f32
    }

    pub fn i64(&self, name: &str) -> i64 {
        self.f64(name).round() as i64
    }

    pub fn bool(&self, name: &str) -> bool {
        match self.get(name) {
            Some(Val::Bool(b)) => *b,
            Some(Val::Num(v)) => *v != 0.0,
            _ => false,
        }
    }

    pub fn str(&self, name: &str) -> &str {
        match self.get(name) {
            Some(Val::Str(s)) => s,
            _ => "",
        }
    }

    pub fn vec4(&self, name: &str) -> [f32; 4] {
        match self.get(name) {
            Some(Val::Vec(v)) => *v,
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    }

    pub fn vec3(&self, name: &str) -> [f32; 3] {
        let v = self.vec4(name);
        [v[0], v[1], v[2]]
    }

    pub fn vec2(&self, name: &str) -> [f32; 2] {
        let v = self.vec4(name);
        [v[0], v[1]]
    }
}

fn set_str(val: &mut Val, s: &str) {
    match val {
        Val::Str(old) if old == s => {}
        Val::Str(old) => {
            old.clear();
            old.push_str(s);
        }
        _ => *val = Val::Str(s.to_owned()),
    }
}

fn from_json(kind: &ParamKind, v: &Value) -> Val {
    match kind {
        ParamKind::Float { .. } | ParamKind::Int { .. } => Val::Num(v.as_f64().unwrap_or(0.0)),
        ParamKind::Toggle | ParamKind::Pulse => Val::Bool(v.as_bool().unwrap_or(false)),
        ParamKind::Colour | ParamKind::Vec { .. } => {
            let mut out = [0.0, 0.0, 0.0, 1.0];
            if let Some(a) = v.as_array() {
                for (o, x) in out.iter_mut().zip(a) {
                    *o = x.as_f64().unwrap_or(0.0) as f32;
                }
            }
            Val::Vec(out)
        }
        ParamKind::String | ParamKind::Menu { .. } | ParamKind::Path => {
            Val::Str(v.as_str().unwrap_or("").to_owned())
        }
    }
}

// ---------------------------------------------------------------- pool --

type Task = Box<dyn FnOnce() + Send>;

/// Stack size of MSH worker threads (the BSP recursion of Boolean).
const STACK: usize = 64 << 20;

fn workers() -> &'static Mutex<Sender<Task>> {
    static POOL: OnceLock<Mutex<Sender<Task>>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));
        let n = std::thread::available_parallelism()
            .map_or(2, |n| n.get().saturating_sub(1))
            .clamp(2, 4);
        for i in 0..n {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("atd-msh-{i}"))
                .stack_size(STACK)
                .spawn(move || {
                    loop {
                        let task = match rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(e) => e.into_inner().recv(),
                        };
                        let Ok(task) = task else { break };
                        // A panicking operator must not take the worker
                        // with it; the job's sender is dropped, which the
                        // node reports.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
                    }
                })
                .expect("starting an MSH worker thread");
        }
        Mutex::new(tx)
    })
}

fn spawn(task: Task) {
    let tx = workers().lock().unwrap_or_else(|e| e.into_inner());
    tx.send(task)
        .expect("MSH workers run for the whole process");
}

// ---------------------------------------------------------------- node --

/// What changes a node's result: its parameters and its inputs.
#[derive(Debug)]
struct Key {
    snap: Snap,
    inputs: Vec<Option<Geometry>>,
}

/// A job ready to run (its inputs readable on the CPU, or read back).
#[derive(Debug)]
struct Spec {
    generation: u64,
    snap: Snap,
    inputs: Vec<Option<Geometry>>,
    /// Mapped readbacks of GPU-only input attributes.
    mapped: Option<Mapped>,
    /// Header copies of GPU-counted inputs (input index, 12 words).
    headers: Vec<(usize, Attribute)>,
}

#[derive(Debug)]
struct Done {
    generation: u64,
    result: Result<Geometry, String>,
    ms: f64,
}

#[derive(Debug)]
struct Running {
    generation: u64,
    cancel: Arc<AtomicBool>,
}

/// The engine-facing GEO operator for every MSH operator.
pub struct MeshNode {
    run: RunFn,
    kinds: Arc<[ParamKind]>,
    inputs: usize,
    scratch: Snap,
    key: Option<Key>,
    generation: u64,
    delivered: u64,
    running: Option<Running>,
    waiting: Option<Spec>,
    reading: Option<Spec>,
    readback: Readback,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    upload: Option<(wgpu::Queue, BufferPool)>,
    error: Option<String>,
    status: String,
    last_delivery: Option<Instant>,
}

impl std::fmt::Debug for MeshNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshNode")
            .field("generation", &self.generation)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// A node delivers its newest result even when superseded if it has shown
/// nothing for this long (so dragging a slider keeps the view moving).
const STARVE: Duration = Duration::from_millis(250);

impl MeshNode {
    pub fn new(schema: &OperatorSchema, run: RunFn) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            run,
            kinds: schema.params.iter().map(|p| p.kind.clone()).collect(),
            inputs: schema.inputs.len(),
            scratch: Snap::empty(schema),
            key: None,
            generation: 0,
            delivered: 0,
            running: None,
            waiting: None,
            reading: None,
            readback: Readback::new(),
            tx,
            rx,
            upload: None,
            error: None,
            status: String::new(),
            last_delivery: None,
        }
    }

    fn start(&mut self, spec: Spec) {
        let Some((queue, pool)) = self.upload.clone() else {
            return;
        };
        let job = Job::default();
        self.running = Some(Running {
            generation: spec.generation,
            cancel: job.cancel.clone(),
        });
        set_status(&mut self.status, "cooking…");
        let (run, tx) = (self.run, self.tx.clone());
        spawn(Box::new(move || {
            let t0 = Instant::now();
            let generation = spec.generation;
            // A bug in an operator becomes a node error, never a hung cook.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute(run, spec, &job, &queue, &pool)
            }))
            .unwrap_or_else(|_| Err("the operator failed unexpectedly (a bug)".into()));
            let _ = tx.send(Done {
                generation,
                result,
                ms: t0.elapsed().as_secs_f64() * 1e3,
            });
        }));
    }

    /// Runs `spec` now or after the running job (latest wins, design M8).
    fn queue(&mut self, spec: Spec) {
        let Some(running) = &self.running else {
            self.start(spec);
            return;
        };
        let recent = self.last_delivery.is_some_and(|t| t.elapsed() < STARVE);
        if recent {
            running.cancel.store(true, Ordering::Relaxed);
        }
        self.waiting = Some(spec);
    }

    fn handle(&mut self, done: Done, out: &mut Geometry) {
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.generation == done.generation)
        {
            self.running = None;
        }
        if done.generation > self.delivered
            && done.result.as_ref().err().is_none_or(|e| e != CANCELLED)
        {
            self.delivered = done.generation;
            self.last_delivery = Some(Instant::now());
            match done.result {
                Ok(g) => {
                    *out = g;
                    self.error = None;
                    set_status(&mut self.status, &format!("cooked in {:.1} ms", done.ms));
                }
                Err(e) => {
                    out.clear();
                    self.error = Some(e);
                    set_status(&mut self.status, "");
                }
            }
        }
        if self.running.is_none()
            && let Some(next) = self.waiting.take()
        {
            self.start(next);
        }
    }

    /// Takes finished results; offline waits until nothing is in flight.
    fn collect(&mut self, mode: CookMode, out: &mut Geometry) -> Result<(), String> {
        // Live: everything that has arrived; offline: until nothing runs.
        while self.running.is_some() {
            match mode.receive(&self.rx) {
                Ok(done) => self.handle(done, out),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("the MSH worker stopped".into());
                }
            }
        }
        Ok(())
    }

    /// Records copies of `inputs`' GPU-only attributes (and the headers of
    /// GPU-counted inputs); returns the header copies, or `None` when every
    /// attribute already has a CPU copy.
    fn record(
        &mut self,
        ctx: &mut GeoCtx<'_>,
        inputs: &[Option<Geometry>],
    ) -> Option<Vec<(usize, Attribute)>> {
        let mut any = false;
        let mut headers = Vec::new();
        for (i, g) in inputs.iter().enumerate() {
            let Some(g) = g else { continue };
            let topo = g
                .topology()
                .into_iter()
                .flat_map(|t| [&t.kinds, &t.start, &t.corners]);
            for a in g
                .attrs(AttrClass::Point)
                .iter()
                .chain(g.attrs(AttrClass::Prim))
                .chain(topo)
            {
                if a.cpu_bytes().is_none() && a.gpu_buffer().is_some() {
                    self.readback.record(ctx.encoder, ctx.buffers, a);
                    any = true;
                }
            }
            if g.is_gpu_counted()
                && let Some(hdr) = g.header()
            {
                let copy = ctx.alloc("__hdr", AttrType::U32, 12);
                let dst = copy.gpu_buffer().expect("allocated on the GPU");
                ctx.encoder.copy_buffer_to_buffer(hdr, 0, dst, 0, 48);
                self.readback.record(ctx.encoder, ctx.buffers, &copy);
                headers.push((i, copy));
                any = true;
            }
        }
        any.then_some(headers)
    }
}

impl Drop for MeshNode {
    fn drop(&mut self) {
        if let Some(r) = &self.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn set_status(status: &mut String, s: &str) {
    if status != s {
        status.clear();
        status.push_str(s);
    }
}

/// Whether two GEO values are the same value (they share every attribute).
fn same(a: Option<&Geometry>, b: Option<&Geometry>) -> bool {
    let (a, b) = match (a, b) {
        (None, None) => return true,
        (Some(a), Some(b)) => (a, b),
        _ => return false,
    };
    let attrs_same = |class| {
        let (x, y) = (a.attrs(class), b.attrs(class));
        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.shares(q))
    };
    let topo_same = match (a.topology(), b.topology()) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.kinds.shares(&y.kinds) && x.start.shares(&y.start) && x.corners.shares(&y.corners)
        }
        _ => false,
    };
    let header_same = match (a.header_handle(), b.header_handle()) {
        (Some(x), Some(y)) => Arc::ptr_eq(x, y) || !a.is_gpu_counted(),
        (None, None) => true,
        _ => !a.is_gpu_counted() && !b.is_gpu_counted(),
    };
    a.points() == b.points()
        && a.is_gpu_counted() == b.is_gpu_counted()
        && attrs_same(AttrClass::Point)
        && attrs_same(AttrClass::Prim)
        && topo_same
        && header_same
        && a.groups(AttrClass::Point) == b.groups(AttrClass::Point)
        && a.groups(AttrClass::Prim) == b.groups(AttrClass::Prim)
}

/// The worker side of a job.
fn execute(
    run: RunFn,
    spec: Spec,
    job: &Job,
    queue: &wgpu::Queue,
    pool: &BufferPool,
) -> Result<Geometry, String> {
    if let Some(m) = spec.mapped {
        for i in 0..m.len() {
            let bytes = m.bytes(i)?;
            m.attribute(i).set_cpu_copy(bytes.into_boxed_slice());
        }
    }
    let mut counts = vec![None; spec.inputs.len()];
    for (i, h) in &spec.headers {
        let b = h.cpu_bytes().ok_or("the header was not read back")?;
        counts[*i] = Some(u32::from_le_bytes(b[..4].try_into().expect("4 bytes")));
    }
    let meshes = spec
        .inputs
        .iter()
        .zip(&counts)
        .map(|(g, &n)| g.as_ref().map(|g| Mesh::from_geometry(g, n)).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    job.check()?;
    let out = run(&spec.snap, &meshes, job)?;
    out.validate()?;
    job.check()?;
    Ok(out.to_geometry(Some((queue, pool))))
}

impl GeoOp for MeshNode {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let mode = ctx.params.mode();
        if self.upload.is_none() {
            self.upload = Some((ctx.queue.clone(), ctx.buffers.clone()));
        }
        self.collect(CookMode::Live, ctx.output)?;
        self.scratch.fill(&self.kinds, &ctx.params);
        let changed = match &self.key {
            None => true,
            Some(k) => {
                k.snap != self.scratch
                    || (0..self.inputs).any(|i| !same(k.inputs[i].as_ref(), ctx.input(i)))
            }
        };
        // One readback at a time: a change while one is in flight is seen
        // again (and taken) once it has finished.
        if changed && self.reading.is_none() {
            let inputs: Vec<Option<Geometry>> =
                (0..self.inputs).map(|i| ctx.input(i).cloned()).collect();
            self.generation += 1;
            self.key = Some(Key {
                snap: self.scratch.clone(),
                inputs: inputs.clone(),
            });
            let mut spec = Spec {
                generation: self.generation,
                snap: self.scratch.clone(),
                inputs,
                mapped: None,
                headers: Vec::new(),
            };
            match self.record(ctx, &spec.inputs) {
                None => self.queue(spec),
                Some(headers) => {
                    spec.headers = headers;
                    if mode.is_offline() {
                        // Run this frame's GPU work (the inputs' producers)
                        // and the copies, then wait for the mapping.
                        let done = std::mem::replace(
                            ctx.encoder,
                            ctx.device.create_command_encoder(&Default::default()),
                        );
                        ctx.queue.submit([done.finish()]);
                        self.readback.start();
                        spec.mapped = self.readback.take_mapped(mode, ctx.device)?;
                        if spec.mapped.is_none() {
                            return Err("an input readback did not finish".into());
                        }
                        self.queue(spec);
                    } else {
                        set_status(&mut self.status, "reading inputs…");
                        self.reading = Some(spec);
                    }
                }
            }
        }
        if self.reading.is_some()
            && let Some(mapped) = self.readback.take_mapped(mode, ctx.device)?
        {
            let mut spec = self.reading.take().expect("checked above");
            spec.mapped = Some(mapped);
            self.queue(spec);
        }
        if mode.is_offline() {
            self.collect(mode, ctx.output)?;
        }
        ctx.cook_again = self.running.is_some() || self.waiting.is_some() || self.reading.is_some();
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn after_submit(&mut self) {
        self.readback.start();
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}
