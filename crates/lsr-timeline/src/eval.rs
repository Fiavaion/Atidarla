//! A timeline bound to a network: keyframe tracks evaluated into live
//! values, and cues fired as the playhead crosses them.
//!
//! [`Timeline::bind`] resolves node IDs, parameter kinds and condition
//! references once (it allocates); after that, [`Timeline::cross`],
//! [`Timeline::finish`] and [`Timeline::evaluate`] allocate nothing
//! (CLAUDE.md rule 2): outputs go to buffers sized at bind time.

use lsr_core::expr::{Env, EvalError, Expr};
use lsr_core::graph::{Network, NodeId};
use lsr_core::registry::{ParamKind, Registry};
use lsr_format::timeline::{CueAction, TimelineDoc};
use serde_json::Value;

use crate::curve;

/// What conditions and vector tracks read from the running patch.
pub trait Signals {
    /// The current value of `channel` of SIG node `node`.
    fn channel(&self, node: NodeId, channel: &str) -> Option<f64>;
    /// The value of `node`'s `param` (component `index` of a vector or
    /// colour), not counting live values: as a binding or expression
    /// drives it, else stored (PRD A66).
    fn param(&self, node: NodeId, param: &str, index: Option<usize>) -> Option<f64>;
}

/// Signals of an empty patch (tests, timelines without conditions).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSignals;

impl Signals for NoSignals {
    fn channel(&self, _: NodeId, _: &str) -> Option<f64> {
        None
    }
    fn param(&self, _: NodeId, _: &str, _: Option<usize>) -> Option<f64> {
        None
    }
}

/// How a parameter takes a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Float,
    Int,
    Bool,
    Vector(usize),
    /// Text, menus, paths: set by cues only.
    Other,
}

/// Where a slot's value comes from this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    None,
    /// The slot's buffer (a track wrote it).
    Buffer,
    /// A value held by a fired cue: (cue, op).
    Op(usize, usize),
}

/// One parameter the timeline drives: its live value this frame.
#[derive(Debug, Clone)]
pub struct Slot {
    pub node: NodeId,
    pub param: String,
    kind: Kind,
    /// A cue's value held until the transport stops: (cue, op).
    latched: Option<(usize, usize)>,
    /// Preallocated output (an array of the right length for vectors).
    buffer: Value,
    source: Source,
    /// The last `target` seen (follow-Stage), for launch slots.
    seen: Option<f64>,
}

/// Which kind of change an op makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// An edit of the stored value.
    Set,
    /// A live value held until stop.
    Live,
    /// A launch: an edit of a layer's `target`.
    Launch,
}

/// One parameter change of a cue, bound to a slot.
#[derive(Debug, Clone)]
pub struct Op {
    pub kind: OpKind,
    pub node: NodeId,
    pub param: String,
    pub value: Value,
    slot: usize,
}

#[derive(Debug, Clone)]
struct Condition {
    expr: Expr,
    /// `op('name')` references resolved to nodes.
    refs: Vec<Option<NodeId>>,
}

#[derive(Debug, Clone)]
struct BoundCue {
    t: Option<f64>,
    cond: Option<Condition>,
    ops: Vec<Op>,
    armed: bool,
    fired: bool,
    /// The condition could not be parsed: the cue never fires.
    never: bool,
    error: Option<EvalError>,
}

#[derive(Debug, Clone, Copy)]
struct BoundTrack {
    /// Index into the document's tracks.
    doc: usize,
    slot: usize,
    component: Option<usize>,
}

/// A timeline bound to a network (see the module docs).
#[derive(Debug, Clone)]
pub struct Timeline {
    doc: TimelineDoc,
    slots: Vec<Slot>,
    tracks: Vec<BoundTrack>,
    cues: Vec<BoundCue>,
    fired: Vec<usize>,
    go: bool,
    current: Option<usize>,
    edits_as_live: bool,
    follow_stage: bool,
    problems: Vec<String>,
    /// Nodes conditions read.
    watched: Vec<NodeId>,
}

impl Timeline {
    /// Binds `doc` to `net`. Tracks and cue actions on nodes or parameters
    /// that do not exist, or cannot take a number, are left out and
    /// reported by [`Self::problems`].
    pub fn bind(doc: TimelineDoc, net: &Network, registry: &Registry) -> Self {
        let mut tl = Self {
            doc: TimelineDoc::default(),
            slots: Vec::new(),
            tracks: Vec::new(),
            cues: Vec::new(),
            fired: Vec::new(),
            go: false,
            current: None,
            edits_as_live: false,
            follow_stage: doc.follow_stage,
            problems: Vec::new(),
            watched: Vec::new(),
        };
        for (i, track) in doc.tracks.iter().enumerate() {
            let at = || format!("track node {} `{}`", track.node.0, track.param);
            let slot = match tl.slot(net, registry, track.node, &track.param) {
                Ok(s) => s,
                Err(e) => {
                    tl.problems.push(format!("{}: {e}", at()));
                    continue;
                }
            };
            let ok = match (tl.slots[slot].kind, track.component) {
                (Kind::Float | Kind::Int | Kind::Bool, None) => Ok(()),
                (Kind::Vector(n), Some(c)) if c < n => Ok(()),
                (Kind::Vector(n), _) => Err(format!("pick a component 0 to {}", n - 1)),
                (Kind::Other, _) => {
                    Err("only numbers, toggles, vectors and colours animate".into())
                }
                (_, Some(_)) => Err("a number has no components".into()),
            };
            match ok {
                Ok(()) => tl.tracks.push(BoundTrack {
                    doc: i,
                    slot,
                    component: track.component,
                }),
                Err(e) => tl.problems.push(format!("{}: {e}", at())),
            }
        }
        for cue in &doc.cues {
            let at = |what: &str| format!("cue {} \"{}\": {what}", cue.id, cue.name);
            let mut ops = Vec::new();
            for action in &cue.actions {
                let changes: Vec<(OpKind, NodeId, &str, Value)> = match action {
                    CueAction::Set { node, param, value } => {
                        vec![(OpKind::Set, *node, param.as_str(), value.clone())]
                    }
                    CueAction::Live { node, param, value } => {
                        vec![(OpKind::Live, *node, param.as_str(), value.clone())]
                    }
                    CueAction::Launch { layers, slot } => layers
                        .iter()
                        .map(|l| (OpKind::Launch, *l, "target", Value::from(*slot)))
                        .collect(),
                };
                for (kind, node, param, value) in changes {
                    match tl.slot(net, registry, node, param) {
                        Ok(slot) => ops.push(Op {
                            kind,
                            node,
                            param: param.to_owned(),
                            value,
                            slot,
                        }),
                        Err(e) => tl.problems.push(at(&format!("node {}: {e}", node.0))),
                    }
                }
            }
            let cond = match cue.when.as_deref().map(Expr::parse) {
                None => None,
                Some(Ok(expr)) => {
                    let refs = expr
                        .refs()
                        .iter()
                        .map(|name| {
                            let id = net.node_by_name(name).map(|n| n.id);
                            if id.is_none() {
                                tl.problems
                                    .push(at(&format!("condition: no node named `{name}`")));
                            }
                            id
                        })
                        .collect();
                    Some(Condition { expr, refs })
                }
                Some(Err(e)) => {
                    tl.problems.push(at(&format!(
                        "condition `{}` is not in the native expression subset ({e}); it never fires",
                        cue.when.as_deref().unwrap_or_default()
                    )));
                    None
                }
            };
            let never = cue.when.is_some() && cond.is_none();
            tl.cues.push(BoundCue {
                t: cue.t,
                cond,
                ops,
                armed: false,
                fired: never,
                never,
                error: None,
            });
        }
        tl.watched = tl
            .cues
            .iter()
            .filter_map(|c| c.cond.as_ref())
            .flat_map(|c| c.refs.iter().flatten().copied())
            .collect();
        tl.watched.sort();
        tl.watched.dedup();
        tl.fired = Vec::with_capacity(tl.cues.len() + 1);
        tl.doc = doc;
        tl.reset();
        tl
    }

    /// The slot for (node, param), created on first use.
    fn slot(
        &mut self,
        net: &Network,
        registry: &Registry,
        node: NodeId,
        param: &str,
    ) -> Result<usize, String> {
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| s.node == node && s.param == param)
        {
            return Ok(i);
        }
        let n = net.node(node).ok_or("no such node")?;
        let schema = registry
            .get(&n.type_id)
            .ok_or_else(|| format!("unknown operator `{}`", n.type_id))?;
        let p = schema
            .params
            .iter()
            .find(|p| p.name == param)
            .ok_or_else(|| format!("{} has no parameter `{param}`", n.type_id))?;
        let width = || p.default.as_array().map_or(4, Vec::len);
        let kind = match &p.kind {
            ParamKind::Float { .. } => Kind::Float,
            ParamKind::Int { .. } => Kind::Int,
            ParamKind::Toggle => Kind::Bool,
            ParamKind::Colour => Kind::Vector(width()),
            ParamKind::Vec { n } => Kind::Vector(usize::from(*n)),
            _ => Kind::Other,
        };
        let buffer = match kind {
            Kind::Vector(n) => Value::Array(vec![Value::from(0.0); n]),
            _ => Value::Null,
        };
        self.slots.push(Slot {
            node,
            param: param.to_owned(),
            kind,
            latched: None,
            buffer,
            source: Source::None,
            seen: None,
        });
        Ok(self.slots.len() - 1)
    }

    /// The document this timeline was bound from.
    pub fn doc(&self) -> &TimelineDoc {
        &self.doc
    }

    /// The nodes conditions read: they must cook every frame (with what
    /// is viewed), or their channels never change.
    pub fn watched(&self) -> &[NodeId] {
        &self.watched
    }

    /// Tracks and cue actions left out, and why.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Offline renders: every cue action holds a live value instead of
    /// editing the network, so a frame depends only on the cues before it
    /// (not on what was rendered earlier).
    pub fn set_edits_as_live(&mut self, yes: bool) {
        self.edits_as_live = yes;
    }

    /// Follow the Stage: launches are the Stage's to make; a cue fires
    /// when the Stage launches what it would have (see
    /// [`Self::observe_stage`]), and its launch is not repeated.
    pub fn set_follow_stage(&mut self, yes: bool) {
        self.follow_stage = yes;
    }

    pub fn follow_stage(&self) -> bool {
        self.follow_stage
    }

    /// Forgets every fired cue and held value (stop, or before a chase).
    /// Manual cues with a condition are armed.
    pub fn reset(&mut self) {
        for c in &mut self.cues {
            // A cue whose condition cannot run counts as fired.
            c.fired = c.never;
            c.armed = !c.never && c.t.is_none() && c.cond.is_some();
            c.error = None;
        }
        for s in &mut self.slots {
            s.latched = None;
            s.source = Source::None;
        }
        self.fired.clear();
        self.go = false;
        self.current = None;
    }

    /// Starts a frame: forgets the cues fired by the last one.
    pub fn begin(&mut self) {
        self.fired.clear();
    }

    /// The playhead moved forward over `(from, to]` (`[from, to]` with
    /// `include_from`, when playback starts): fires the timed cues in it
    /// (exactly once), and arms those with a condition.
    pub fn cross(&mut self, from: f64, to: f64, include_from: bool) {
        for i in 0..self.cues.len() {
            let c = &self.cues[i];
            let Some(t) = c.t else { continue };
            let inside = (t > from || (include_from && t == from)) && t <= to;
            if !inside || c.fired {
                continue;
            }
            if c.cond.is_some() {
                self.cues[i].armed = true;
            } else {
                self.fire(i);
            }
        }
    }

    /// Lets timed cues in `[from, to]` (`(from, to]` without
    /// `include_from`) fire again: after a loop wraps or the playhead jumps
    /// back. Values they hold stay until replaced or stopped.
    pub fn rearm(&mut self, from: f64, to: f64, include_from: bool) {
        for c in &mut self.cues {
            let Some(t) = c.t else { continue };
            if (t > from || (include_from && t == from)) && t <= to && !c.never {
                c.fired = false;
                c.armed = false;
            }
        }
    }

    /// Fires the next manual cue on the next [`Self::finish`].
    pub fn go(&mut self) {
        self.go = true;
    }

    /// Ends a frame at time `t`: fires armed cues whose condition holds and
    /// a pending Go. Returns every cue fired this frame, in order.
    pub fn finish(&mut self, t: f64, signals: &dyn Signals) -> &[usize] {
        let frame = (t * self.doc.fps).floor();
        for i in 0..self.cues.len() {
            let c = &self.cues[i];
            if !c.armed || c.fired {
                continue;
            }
            let Some(cond) = &c.cond else { continue };
            let env = CondEnv {
                refs: &cond.refs,
                signals,
                time: t,
                frame,
            };
            match cond.expr.eval(&env) {
                Ok(v) if v != 0.0 && !v.is_nan() => {
                    self.cues[i].armed = false;
                    self.cues[i].error = None;
                    self.fire(i);
                }
                Ok(_) => self.cues[i].error = None,
                Err(e) => self.cues[i].error = Some(e),
            }
        }
        if std::mem::take(&mut self.go) {
            // The next manual cue after the current one, else the first
            // not yet fired.
            let after = self.current.map_or(0, |c| c + 1);
            let manual = |i: &usize| self.cues[*i].t.is_none() && !self.cues[*i].fired;
            let next = (after..self.cues.len())
                .find(manual)
                .or_else(|| (0..self.cues.len()).find(manual));
            if let Some(i) = next {
                self.cues[i].armed = false;
                self.fire(i);
            }
        }
        &self.fired
    }

    /// Follow-Stage: reads each launch slot's `target` and, when the Stage
    /// changed one, fires the next cue that launches that slot there (its
    /// launch is not repeated). Returns the fired cue's time, for the
    /// playhead to jump to.
    pub fn observe_stage(&mut self, signals: &dyn Signals) -> Option<f64> {
        let mut jump = None;
        for s in 0..self.slots.len() {
            if self.slots[s].param != "target" {
                continue;
            }
            let now = signals.param(self.slots[s].node, "target", None);
            let before = std::mem::replace(&mut self.slots[s].seen, now);
            let (Some(now), Some(before)) = (now, before) else {
                continue;
            };
            if now == before {
                continue;
            }
            let launches = |c: &BoundCue| {
                !c.fired
                    && c.ops.iter().any(|o| {
                        o.kind == OpKind::Launch && o.slot == s && o.value.as_f64() == Some(now)
                    })
            };
            let after = self.current.map_or(0, |c| c + 1);
            let next = (after..self.cues.len())
                .find(|&i| launches(&self.cues[i]))
                .or_else(|| (0..self.cues.len()).find(|&i| launches(&self.cues[i])));
            if let Some(i) = next {
                self.cues[i].armed = false;
                self.fire(i);
                jump = self.cues[i].t.or(jump);
            }
        }
        jump
    }

    fn fire(&mut self, i: usize) {
        let c = &mut self.cues[i];
        c.fired = true;
        if !self.fired.contains(&i) {
            self.fired.push(i);
        }
        self.current = Some(i);
        for (o, op) in self.cues[i].ops.iter().enumerate() {
            let hold = match op.kind {
                OpKind::Live => true,
                OpKind::Launch if self.follow_stage => false,
                OpKind::Set | OpKind::Launch => self.edits_as_live,
            };
            if hold {
                self.slots[op.slot].latched = Some((i, o));
            }
        }
    }

    /// Restarts from nothing and fires every timed cue up to `t` (without
    /// waiting for conditions: those are armed), so the cue state matches
    /// playing from the start. Returns the cues fired.
    pub fn chase(&mut self, t: f64) -> &[usize] {
        self.reset();
        self.cross(f64::NEG_INFINITY, t, true);
        &self.fired
    }

    /// Writes every slot's value at time `t`: tracks win over values held
    /// by cues. Unkeyed components of a vector take the held value, else
    /// the stored one (`signals`).
    pub fn evaluate(&mut self, t: f64, signals: &dyn Signals) -> &[Slot] {
        for s in &mut self.slots {
            s.source = s.latched.map_or(Source::None, |(c, o)| Source::Op(c, o));
        }
        for bt in &self.tracks {
            let keys = &self.doc.tracks[bt.doc].keys;
            let Some(v) = curve::sample(keys, t) else {
                continue;
            };
            let slot = &mut self.slots[bt.slot];
            match (slot.kind, bt.component) {
                (Kind::Float, _) => slot.buffer = Value::from(v),
                (Kind::Int, _) => slot.buffer = Value::from(v.round() as i64),
                (Kind::Bool, _) => slot.buffer = Value::Bool(v >= 0.5),
                (Kind::Vector(n), Some(c)) => {
                    if slot.source != Source::Buffer {
                        let held = match slot.source {
                            Source::Op(ci, oi) => Some(&self.cues[ci].ops[oi].value),
                            _ => None,
                        };
                        if let Value::Array(a) = &mut slot.buffer {
                            for (i, x) in a.iter_mut().enumerate().take(n) {
                                let base = held
                                    .and_then(|h| h.get(i))
                                    .and_then(Value::as_f64)
                                    .or_else(|| signals.param(slot.node, &slot.param, Some(i)))
                                    .unwrap_or(0.0);
                                *x = Value::from(base);
                            }
                        }
                    }
                    if let Some(x) = slot.buffer.get_mut(c) {
                        *x = Value::from(v);
                    }
                }
                _ => continue,
            }
            slot.source = Source::Buffer;
        }
        &self.slots
    }

    /// The cues fired since the last [`Self::begin`], in order.
    pub fn fired_now(&self) -> &[usize] {
        &self.fired
    }

    /// Every slot, with the values of the last [`Self::evaluate`].
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// Slot `s`'s live value this frame; `None` clears it.
    pub fn value(&self, s: usize) -> Option<&Value> {
        let slot = &self.slots[s];
        match slot.source {
            Source::None => None,
            Source::Buffer => Some(&slot.buffer),
            Source::Op(c, o) => Some(&self.cues[c].ops[o].value),
        }
    }

    /// Cue `i`'s changes (bound to existing nodes).
    pub fn ops(&self, i: usize) -> &[Op] {
        &self.cues[i].ops
    }

    /// The cue fired last (the cue list's position).
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Whether cue `i` has fired since the last reset.
    pub fn fired(&self, i: usize) -> bool {
        self.cues[i].fired
    }

    /// Whether cue `i` is armed (waiting for its condition).
    pub fn armed(&self, i: usize) -> bool {
        self.cues[i].armed
    }

    /// Why cue `i`'s condition failed on its last evaluation.
    pub fn cue_error(&self, i: usize) -> Option<String> {
        let c = &self.cues[i];
        Some(c.error?.message(&c.cond.as_ref()?.expr))
    }
}

impl Slot {
    /// Whether this slot's parameter is set by tracks or cues to a number
    /// kind (as opposed to text or menus).
    pub fn animates(&self) -> bool {
        self.kind != Kind::Other
    }
}

struct CondEnv<'a> {
    refs: &'a [Option<NodeId>],
    signals: &'a dyn Signals,
    time: f64,
    frame: f64,
}

impl Env for CondEnv<'_> {
    fn time(&self) -> f64 {
        self.time
    }
    fn frame(&self) -> f64 {
        self.frame
    }
    fn me_digits(&self) -> Option<f64> {
        None
    }
    fn param(&self, node: usize, param: &str, index: Option<usize>) -> Result<f64, EvalError> {
        let id = self.refs[node].ok_or(EvalError::NoNode(node))?;
        self.signals
            .param(id, param, index)
            .ok_or(EvalError::NoParam(node))
    }
    fn channel(&self, node: usize, channel: &str) -> Result<f64, EvalError> {
        let id = self.refs[node].ok_or(EvalError::NoNode(node))?;
        self.signals
            .channel(id, channel)
            .ok_or(EvalError::NoChannel(node))
    }
}
