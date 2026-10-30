//! A timeline playing on an [`Engine`]: each frame the transport moves,
//! cues fire, and tracks become live values (`Engine::set_live`, decision
//! A54) — above bindings, expressions and stored values, never saved.
//!
//! Live (the app): a cue's `set` and `launch` actions are edits (one undo
//! step per cue, saved); `live` actions hold live values until stop.
//! Offline ([`Player::offline`], `atidarla render`): every action holds a
//! live value, so a frame depends on the cues before its time, not on what
//! was rendered before it.

use atd_core::graph::{Edit, NodeId, Param, assign_value};
use atd_format::timeline::TimelineDoc;
use atd_gpu::engine::Engine;
use serde_json::Value;

use crate::eval::{OpKind, Signals, Timeline};
use crate::transport::{ClockSource, Motion, PlayState, Transport};

/// The engine's signals: SIG channels from the last cook, stored parameter
/// values from the network.
#[derive(Debug, Clone, Copy)]
pub struct EngineSignals<'a>(pub &'a Engine);

impl Signals for EngineSignals<'_> {
    fn channel(&self, node: NodeId, channel: &str) -> Option<f64> {
        self.0.channels(node)?.value(channel).map(f64::from)
    }

    fn param(&self, node: NodeId, param: &str, index: Option<usize>) -> Option<f64> {
        let v = self
            .0
            .network()
            .param_value(self.0.registry(), node, param)?;
        let v = match index {
            Some(i) => v.get(i)?,
            None => v,
        };
        v.as_f64()
            .or_else(|| v.as_bool().map(|b| f64::from(u8::from(b))))
    }
}

/// A timeline driving an engine (see the module docs).
#[derive(Debug)]
pub struct Player {
    pub transport: Transport,
    timeline: Timeline,
    /// Per slot: the live value last given to the engine.
    applied: Vec<Option<Value>>,
    /// Offline: the time of the last frame rendered.
    rendered: Option<f64>,
    offline: bool,
    /// The Timecode In node chased (the document's `clock`).
    timecode_node: Option<NodeId>,
    /// Nodes to cook every frame: condition references and the Timecode
    /// In node.
    watched: Vec<NodeId>,
    /// Problems from the last edit a cue made.
    pub last_error: Option<String>,
}

impl Player {
    /// A stopped player for `doc` on `engine`'s network, on the clock and
    /// in the Stage mode the document asks for.
    pub fn new(doc: TimelineDoc, engine: &Engine) -> Self {
        let mut transport = Transport::new(doc.length);
        transport.loop_region = doc.loop_region;
        transport.looping = doc.loop_region.is_some();
        let timeline = Timeline::bind(doc, engine.network(), engine.registry());
        Self {
            transport,
            applied: vec![None; timeline.slots().len()],
            timeline,
            rendered: None,
            offline: false,
            timecode_node: None,
            watched: Vec::new(),
            last_error: None,
        }
        .configured()
    }

    fn configured(mut self) -> Self {
        self.configure();
        self
    }

    /// Takes the clock (internal, or chasing a Timecode In node with an
    /// offset) and the Stage mode from the document.
    fn configure(&mut self) {
        let doc = self.timeline.doc();
        let (follow, clock) = (doc.follow_stage, doc.clock);
        self.timeline.set_follow_stage(follow);
        let node = clock.map(|c| c.node);
        if node != self.timecode_node {
            self.transport.chase.reset();
        }
        self.transport.clock = match clock {
            Some(c) => {
                self.transport.chase.config.offset = c.offset;
                ClockSource::Timecode
            }
            None => ClockSource::Internal,
        };
        self.set_timecode_node(node);
    }

    fn rewatch(&mut self) {
        self.watched.clear();
        self.watched.extend_from_slice(self.timeline.watched());
        self.watched.extend(self.timecode_node);
    }

    /// Chase a Timecode In node's `time` channel while its `locked` channel
    /// is on (the node reads MTC or LTC on its own thread); `None` stops.
    /// Set from the document's `clock` (see `configure`).
    fn set_timecode_node(&mut self, node: Option<NodeId>) {
        self.timecode_node = node;
        self.rewatch();
    }

    pub fn timecode_node(&self) -> Option<NodeId> {
        self.timecode_node
    }

    /// Nodes the timeline reads, to cook with the frame (conditions'
    /// references, the chased Timecode In node): nothing else may need
    /// them.
    pub fn watched(&self) -> &[NodeId] {
        &self.watched
    }

    /// A player for offline renders: drive it with [`Self::render_at`].
    /// Offline, cues drive the Stage whatever the document's Stage mode
    /// (there is no performer to follow).
    pub fn offline(doc: TimelineDoc, engine: &Engine) -> Self {
        let mut p = Self::new(doc, engine);
        p.offline = true;
        p.timeline.set_edits_as_live(true);
        p.timeline.set_follow_stage(false);
        p
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Fires the next manual cue on the next tick.
    pub fn go(&mut self) {
        self.timeline.go();
    }

    /// Replaces the document (after an edit, or when the network changed):
    /// values of parameters no longer driven are cleared, and while playing,
    /// cues before the playhead hold their values again (no edits repeat).
    /// The clock and Stage mode follow the new document.
    pub fn rebind(&mut self, doc: TimelineDoc, engine: &mut Engine) {
        self.transport.length = doc.length;
        self.transport.loop_region = doc.loop_region;
        let mut next = Timeline::bind(doc, engine.network(), engine.registry());
        next.set_edits_as_live(self.offline);
        let mut applied = vec![None; next.slots().len()];
        for (old, value) in self.timeline.slots().iter().zip(&mut self.applied) {
            let Some(value) = value.take() else { continue };
            match next
                .slots()
                .iter()
                .position(|s| s.node == old.node && s.param == old.param)
            {
                Some(i) => applied[i] = Some(value),
                None => {
                    engine.clear_live_if(old.node, &old.param, &value);
                }
            }
        }
        if self.transport.state() != PlayState::Stopped {
            next.chase(self.transport.time());
        }
        self.timeline = next;
        self.applied = applied;
        self.configure();
        if self.offline {
            self.timeline.set_follow_stage(false);
        }
        self.apply(engine);
    }

    /// One app frame at local time `now` (seconds): moves the transport,
    /// fires cues (edits as one undo step per cue) and sets live values.
    pub fn tick(&mut self, engine: &mut Engine, now: f64) {
        if self.transport.clock == ClockSource::Timecode
            && let Some(node) = self.timecode_node
            && let Some(ch) = engine.channels(node)
            && ch.value("locked").is_some_and(|l| l > 0.5)
            && let Some(time) = ch.value("time")
        {
            self.transport.chase.reading(f64::from(time), now, 0.0);
        }
        let motion = self.transport.advance(now);
        if self.transport.state() == PlayState::Stopped {
            self.clear(engine);
            return;
        }
        self.timeline.begin();
        match motion {
            Motion::Still => {}
            Motion::Forward {
                from,
                to,
                include_from,
            } => self.timeline.cross(from, to, include_from),
            Motion::Wrapped {
                from,
                end,
                start,
                to,
            } => {
                self.timeline.cross(from, end, false);
                self.timeline.rearm(start, end, true);
                self.timeline.cross(start, to, true);
            }
            Motion::Jumped { to, chase: true } => {
                self.timeline.chase(to);
            }
            Motion::Jumped { to, chase: false } => {
                self.timeline.rearm(to, f64::INFINITY, true);
            }
        }
        if self.timeline.follow_stage()
            && let Some(at) = self.timeline.observe_stage(&EngineSignals(engine))
        {
            // The Stage launched this cue: the playhead goes to it, and
            // cues after it can fire again.
            self.timeline.rearm(at, f64::INFINITY, false);
            self.transport.relocate(at);
        }
        let t = self.transport.time();
        self.timeline.finish(t, &EngineSignals(engine));
        self.edit_fired(engine);
        self.timeline.evaluate(t, &EngineSignals(engine));
        self.apply(engine);
    }

    /// Offline: the timeline at time `t` for the frame about to cook.
    /// Frames rendered in order fire cues as crossed (conditions read the
    /// previous frame's channels); any other frame first fires every timed
    /// cue before it.
    pub fn render_at(&mut self, engine: &mut Engine, t: f64) {
        self.timeline.begin();
        match self.rendered {
            Some(prev) if t >= prev => self.timeline.cross(prev, t, false),
            _ => {
                self.timeline.chase(t);
            }
        }
        self.rendered = Some(t);
        self.timeline.finish(t, &EngineSignals(engine));
        self.timeline.evaluate(t, &EngineSignals(engine));
        self.apply(engine);
    }

    /// Stops: clears every live value the timeline set and forgets fired
    /// cues. A value another writer (a mapping) set since stays.
    pub fn stop(&mut self, engine: &mut Engine) {
        self.transport.stop();
        self.clear(engine);
    }

    fn clear(&mut self, engine: &mut Engine) {
        for (slot, value) in self.timeline.slots().iter().zip(&mut self.applied) {
            if let Some(v) = value.take() {
                engine.clear_live_if(slot.node, &slot.param, &v);
            }
        }
        self.timeline.reset();
    }

    /// Edits for the cues fired this frame (live mode): one undo step each.
    fn edit_fired(&mut self, engine: &mut Engine) {
        if self.offline {
            return;
        }
        for k in 0..self.timeline.fired_now().len() {
            let i = self.timeline.fired_now()[k];
            let follow = self.timeline.follow_stage();
            let edits: Vec<Edit> = self
                .timeline
                .ops(i)
                .iter()
                .filter(|op| match op.kind {
                    OpKind::Set => true,
                    OpKind::Launch => !follow,
                    OpKind::Live => false,
                })
                .map(|op| Edit::SetParam {
                    node: op.node,
                    param: op.param.clone(),
                    value: Some(Param::constant(op.value.clone())),
                })
                .collect();
            if edits.is_empty() {
                continue;
            }
            let name = &self.timeline.doc().cues[i].name;
            let label = if name.is_empty() {
                "Cue".to_owned()
            } else {
                format!("Cue {name}")
            };
            if let Err(e) = engine.edit(&label, edits) {
                self.last_error = Some(format!("{label}: {e}"));
            }
        }
    }

    /// Gives the engine every slot's value; only changes are sent. A
    /// number or colour changing every frame allocates nothing.
    fn apply(&mut self, engine: &mut Engine) {
        for (s, slot) in self.timeline.slots().iter().enumerate() {
            let want = self.timeline.value(s);
            if want == self.applied[s].as_ref() {
                continue;
            }
            match (want, &mut self.applied[s]) {
                (Some(w), Some(a)) => {
                    engine.set_live(slot.node, &slot.param, Some(w));
                    assign_value(a, w);
                }
                (Some(w), a) => {
                    engine.set_live(slot.node, &slot.param, Some(w));
                    *a = Some(w.clone());
                }
                (None, a) => {
                    if let Some(prev) = a.take() {
                        engine.clear_live_if(slot.node, &slot.param, &prev);
                    }
                }
            }
        }
    }
}
