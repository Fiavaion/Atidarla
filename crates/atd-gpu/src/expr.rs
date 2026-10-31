//! Parameter expressions in the engine (FR-P2, PRD A22).
//!
//! [`ExprTable`] holds every expression in the network, compiled when the
//! network changes: native ones parsed, `op('…')` names resolved to node
//! IDs, Python ones keyed for the worker. Each frame, before a node cooks,
//! its native expressions are evaluated (allocating nothing) and the values
//! kept for [`crate::engine::Params`]; Python results latched from the
//! worker are used as they arrive, a frame late.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use atd_core::cook::Extra;
use atd_core::expr::{self, EvalError, Expr, PyJob, PyRef};
use atd_core::graph::{Network, NodeId};
use atd_core::registry::{ParamKind, Registry};
use serde_json::Value;

use crate::engine::Channels;

/// References nested deeper than this are a loop.
const MAX_DEPTH: u8 = 8;

#[derive(Debug)]
pub(crate) enum Tier {
    Native {
        expr: Expr,
        /// `expr.refs()` resolved; `None` for a name with no node.
        refs: Vec<Option<NodeId>>,
    },
    Python {
        key: u64,
        refs: Vec<(Arc<str>, Option<NodeId>)>,
    },
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub param: String,
    pub source: Arc<str>,
    pub tier: Tier,
    /// This frame's value (native) or the latest from Python.
    pub value: Cell<Option<f64>>,
    /// The current error, formatted only when it changes.
    pub error: RefCell<Option<String>>,
    last_error: Cell<Option<EvalError>>,
}

/// Every expression in the network.
#[derive(Debug, Default)]
pub struct ExprTable {
    nodes: HashMap<NodeId, Vec<Entry>>,
    next_key: u64,
}

impl ExprTable {
    /// Compiles the expressions of `net`, carrying values over from `old`
    /// where the source is unchanged. Returns the table, the scheduler's
    /// extra dependencies, and the nodes whose expressions changed.
    pub(crate) fn build(
        net: &Network,
        old: &mut ExprTable,
    ) -> (ExprTable, HashMap<NodeId, Extra>, Vec<NodeId>) {
        let mut table = ExprTable {
            nodes: HashMap::new(),
            next_key: old.next_key,
        };
        let mut extra = HashMap::new();
        let mut changed = Vec::new();
        let resolve = |name: &str| net.node_by_name(name).map(|n| n.id);
        for node in net.nodes() {
            let mut entries = Vec::new();
            let mut deps = Vec::new();
            let mut time = false;
            for (param, p) in &node.params {
                let Some(source) = p.expr.as_deref() else {
                    continue;
                };
                let previous = old
                    .nodes
                    .get_mut(&node.id)
                    .and_then(|list| {
                        let i = list.iter().position(|e| e.param == *param)?;
                        Some(list.swap_remove(i))
                    })
                    .filter(|e| *e.source == *source);
                let tier = match Expr::parse(source) {
                    Ok(expr) => {
                        let refs: Vec<Option<NodeId>> =
                            expr.refs().iter().map(|r| resolve(r)).collect();
                        time |= expr.uses_time();
                        Tier::Native { expr, refs }
                    }
                    Err(_) => {
                        let key = match previous.as_ref().map(|e| &e.tier) {
                            Some(Tier::Python { key, .. }) => *key,
                            _ => {
                                table.next_key += 1;
                                table.next_key
                            }
                        };
                        let refs = expr::python_refs(source)
                            .into_iter()
                            .map(|r| {
                                let id = resolve(&r);
                                (Arc::from(r.as_str()), id)
                            })
                            .collect();
                        // Python can read anything: run it every frame.
                        time = true;
                        Tier::Python { key, refs }
                    }
                };
                let resolved: Vec<NodeId> = match &tier {
                    Tier::Native { refs, .. } => refs.iter().flatten().copied().collect(),
                    Tier::Python { refs, .. } => refs.iter().filter_map(|r| r.1).collect(),
                };
                deps.extend(resolved.iter().copied());
                let same = previous.as_ref().is_some_and(|prev| {
                    let before: Vec<NodeId> = match &prev.tier {
                        Tier::Native { refs, .. } => refs.iter().flatten().copied().collect(),
                        Tier::Python { refs, .. } => refs.iter().filter_map(|r| r.1).collect(),
                    };
                    before == resolved
                });
                if !same {
                    changed.push(node.id);
                }
                let (value, error) = previous
                    .map(|e| (e.value.get(), e.error.into_inner()))
                    .unwrap_or_default();
                entries.push(Entry {
                    param: param.clone(),
                    source: Arc::from(source),
                    tier,
                    value: Cell::new(value),
                    error: RefCell::new(error),
                    last_error: Cell::new(None),
                });
            }
            if !entries.is_empty() {
                deps.sort();
                deps.dedup();
                extra.insert(
                    node.id,
                    Extra {
                        deps,
                        time_dependent: time,
                    },
                );
                table.nodes.insert(node.id, entries);
            }
        }
        changed.sort();
        changed.dedup();
        (table, extra, changed)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn entry(&self, node: NodeId, param: &str) -> Option<&Entry> {
        self.nodes.get(&node)?.iter().find(|e| e.param == param)
    }

    /// The value an expression gave a parameter this frame.
    pub(crate) fn value(&self, node: NodeId, param: &str) -> Option<f64> {
        self.entry(node, param)?.value.get()
    }

    /// The first expression error on `node`, if any.
    pub(crate) fn error(&self, node: NodeId) -> Option<std::cell::Ref<'_, String>> {
        self.nodes
            .get(&node)?
            .iter()
            .find_map(|e| std::cell::Ref::filter_map(e.error.borrow(), Option::as_ref).ok())
    }

    /// Marks Python expressions as unavailable (no worker attached).
    pub(crate) fn no_python(&self) {
        for e in self.nodes.values().flatten() {
            if matches!(e.tier, Tier::Python { .. }) && e.error.borrow().is_none() {
                e.error.replace(Some(format!(
                    "expression for `{}` needs Python, which is not running here",
                    e.param
                )));
            }
        }
    }

    /// Applies results from the Python worker.
    pub(crate) fn apply_python(&self, results: &mut Vec<(u64, Result<f64, String>)>) {
        if results.is_empty() {
            return;
        }
        for entries in self.nodes.values() {
            for e in entries {
                let Tier::Python { key, .. } = &e.tier else {
                    continue;
                };
                // The latest result for this key wins.
                let Some(i) = results.iter().rposition(|(k, _)| k == key) else {
                    continue;
                };
                let (_, result) = results.swap_remove(i);
                results.retain(|(k, _)| k != key);
                match result {
                    Ok(v) if v.is_finite() => {
                        e.value.set(Some(v));
                        e.error.replace(None);
                    }
                    Ok(_) => {
                        e.value.set(None);
                        e.error.replace(Some(format!(
                            "expression for `{}`: not a finite number",
                            e.param
                        )));
                    }
                    Err(msg) => {
                        e.value.set(None);
                        e.error
                            .replace(Some(format!("expression for `{}`: {msg}", e.param)));
                    }
                }
            }
        }
    }

    /// Rebuilds `jobs` for the Python expressions (when the table changed).
    pub(crate) fn python_jobs(&self, net: &Network, jobs: &mut Vec<PyJob>) {
        jobs.clear();
        let mut ids: Vec<&NodeId> = self.nodes.keys().collect();
        ids.sort();
        for id in ids {
            let name: Arc<str> = net.node(*id).map_or("", |n| n.name.as_str()).into();
            for e in &self.nodes[id] {
                let Tier::Python { key, refs } = &e.tier else {
                    continue;
                };
                jobs.push(PyJob {
                    key: *key,
                    source: e.source.clone(),
                    time: 0.0,
                    frame: 0.0,
                    me: name.clone(),
                    refs: refs
                        .iter()
                        .map(|(n, id)| PyRef {
                            name: n.clone(),
                            exists: id.is_some(),
                            params: Vec::new(),
                            channels: Vec::new(),
                        })
                        .collect(),
                });
            }
        }
    }
}

/// Everything an expression can read this frame.
pub(crate) struct Ctx<'a> {
    pub net: &'a Network,
    pub registry: &'a Registry,
    pub sig: &'a HashMap<NodeId, Channels>,
    pub table: &'a ExprTable,
    /// Live values win over everything else, as in `Params`.
    pub live: &'a crate::engine::LiveValues,
    pub time: f64,
    pub frame: f64,
}

impl Ctx<'_> {
    /// Evaluates `node`'s native expressions, keeping values and errors.
    pub(crate) fn eval_node(&self, node: NodeId) {
        let Some(entries) = self.table.nodes.get(&node) else {
            return;
        };
        for e in entries {
            let Tier::Native { expr, refs } = &e.tier else {
                continue;
            };
            let result = self.eval(node, expr, refs, 0).and_then(|v| {
                if v.is_finite() {
                    Ok(v)
                } else {
                    Err(EvalError::Domain("result"))
                }
            });
            match result {
                Ok(v) => {
                    e.value.set(Some(v));
                    if e.last_error.take().is_some() {
                        e.error.replace(None);
                    }
                }
                Err(err) => {
                    // The parameter falls back to its stored value.
                    e.value.set(None);
                    if e.last_error.get() != Some(err) {
                        e.last_error.set(Some(err));
                        let mut message = err.message(expr);
                        // Name the channels there are (formatted only when
                        // the error changes, never per frame).
                        if let EvalError::NoChannel(i) = err
                            && let Some(c) = refs
                                .get(i)
                                .copied()
                                .flatten()
                                .and_then(|id| self.sig.get(&id))
                        {
                            let names: Vec<&str> = c.names().collect();
                            message.push_str(&format!("; it has: {}", names.join(", ")));
                        }
                        e.error
                            .replace(Some(format!("expression for `{}`: {message}", e.param)));
                    }
                }
            }
        }
    }

    fn eval(
        &self,
        node: NodeId,
        expr: &Expr,
        refs: &[Option<NodeId>],
        depth: u8,
    ) -> Result<f64, EvalError> {
        let me = self.net.node(node).and_then(|n| expr::name_digits(&n.name));
        expr.eval(&Env {
            ctx: self,
            refs,
            me,
            depth,
        })
    }

    /// A parameter's number: its live value, binding, expression or stored
    /// value. An expression that fails falls back to the stored value, as on
    /// its own node; a reference loop is an error.
    fn number(
        &self,
        node: NodeId,
        param: &str,
        index: Option<usize>,
        depth: u8,
    ) -> Result<f64, NumError> {
        let n = self.net.node(node).ok_or(NumError::Missing)?;
        let live = self.live.get(node, param);
        if index.is_none() && live.is_none() {
            if let Some(bind) = n.params.get(param).and_then(|p| p.bind.as_ref())
                && let Some(v) = self
                    .sig
                    .get(&bind.node)
                    .and_then(|c| c.value(&bind.channel))
            {
                return Ok(f64::from(v));
            }
            if let Some(e) = self.table.entry(node, param) {
                match &e.tier {
                    Tier::Native { expr, refs } => {
                        if depth >= MAX_DEPTH {
                            return Err(NumError::Loop);
                        }
                        match self.eval(node, expr, refs, depth + 1) {
                            Ok(v) => return Ok(v),
                            Err(EvalError::TooDeep) => return Err(NumError::Loop),
                            Err(_) => {}
                        }
                    }
                    Tier::Python { .. } => {
                        if let Some(v) = e.value.get() {
                            return Ok(v);
                        }
                    }
                }
            }
        }
        let value = live
            .or_else(|| self.net.param_value(self.registry, node, param))
            .ok_or(NumError::Missing)?;
        let scalar = |v: &Value| {
            v.as_f64()
                .or_else(|| v.as_bool().map(|b| f64::from(u8::from(b))))
        };
        match (value, index) {
            (Value::Array(a), Some(i)) => a.get(i).and_then(scalar).ok_or(NumError::Index),
            (v, None) => scalar(v).ok_or(NumError::Missing),
            _ => Err(NumError::Index),
        }
    }

    /// Refreshes `jobs` (built by [`ExprTable::python_jobs`]) with this
    /// frame's time and the values of referenced nodes. Reuses buffers; it
    /// allocates only when a referenced node's channels change names.
    pub(crate) fn update_python_jobs(&self, jobs: &mut [PyJob]) {
        for job in jobs {
            job.time = self.time;
            job.frame = self.frame;
            for r in &mut job.refs {
                let Some(node) = self.net.node_by_name(&r.name) else {
                    r.exists = false;
                    continue;
                };
                r.exists = true;
                if let Some(schema) = self.registry.get(&node.type_id) {
                    let numeric = schema.params.iter().filter(|p| {
                        matches!(
                            p.kind,
                            ParamKind::Float { .. }
                                | ParamKind::Int { .. }
                                | ParamKind::Toggle
                                | ParamKind::Vec { .. }
                                | ParamKind::Colour
                        )
                    });
                    let count = numeric.clone().count();
                    if r.params.len() != count {
                        r.params = numeric
                            .map(|p| (Arc::from(p.name.as_str()), Vec::new()))
                            .collect();
                    }
                    for (name, values) in &mut r.params {
                        values.clear();
                        let value = self
                            .live
                            .get(node.id, name)
                            .or_else(|| self.net.param_value(self.registry, node.id, name));
                        match value {
                            Some(Value::Array(a)) => {
                                values.extend(a.iter().map(|v| v.as_f64().unwrap_or(0.0)));
                            }
                            Some(_) => {
                                if let Ok(v) = self.number(node.id, name, None, 0) {
                                    values.push(v);
                                }
                            }
                            None => {}
                        }
                    }
                }
                match self.sig.get(&node.id) {
                    Some(ch) => {
                        let same = r.channels.len() == ch.len()
                            && r.channels
                                .iter()
                                .zip(ch.names())
                                .all(|((a, _), b)| **a == *b);
                        if !same {
                            r.channels = ch.names().map(|n| (Arc::from(n), 0.0)).collect();
                        }
                        for (c, (_, v)) in r.channels.iter_mut().enumerate() {
                            *v = f64::from(ch.last(c));
                        }
                    }
                    None => r.channels.clear(),
                }
            }
        }
    }
}

enum NumError {
    Missing,
    Index,
    Loop,
}

struct Env<'a> {
    ctx: &'a Ctx<'a>,
    refs: &'a [Option<NodeId>],
    me: Option<f64>,
    depth: u8,
}

impl expr::Env for Env<'_> {
    fn time(&self) -> f64 {
        self.ctx.time
    }
    fn frame(&self) -> f64 {
        self.ctx.frame
    }
    fn me_digits(&self) -> Option<f64> {
        self.me
    }
    fn param(&self, node: usize, param: &str, index: Option<usize>) -> Result<f64, EvalError> {
        let id = self.refs[node].ok_or(EvalError::NoNode(node))?;
        if self.depth >= MAX_DEPTH {
            return Err(EvalError::TooDeep);
        }
        self.ctx
            .number(id, param, index, self.depth + 1)
            .map_err(|e| match e {
                NumError::Missing => EvalError::NoParam(node),
                NumError::Index => EvalError::Index(node),
                NumError::Loop => EvalError::TooDeep,
            })
    }
    fn channel(&self, node: usize, channel: &str) -> Result<f64, EvalError> {
        let id = self.refs[node].ok_or(EvalError::NoNode(node))?;
        self.ctx
            .sig
            .get(&id)
            .and_then(|c| c.value(channel))
            .map(f64::from)
            .ok_or(EvalError::NoChannel(node))
    }
}
