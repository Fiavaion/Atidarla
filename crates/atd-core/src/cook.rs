//! Pull-cook scheduler (PRD §5.3).
//!
//! Consumers (viewers, outputs, exports) request nodes each frame. The
//! [`Scheduler`] walks upstream from them and plans, in dependency order,
//! the nodes that must cook: those that are dirty, never cooked,
//! time-dependent, fed by a feedback input, or downstream of a node cooking
//! this frame. Everything else reuses its cached result.
//!
//! Edits mark nodes dirty through [`Scheduler::mark_dirty`], which also
//! dirties everything downstream. Executors call [`Scheduler::record`]
//! after cooking a node, which clears its dirty flag and updates its cook
//! metrics (always on).
//!
//! [`Scheduler::plan`] reuses internal buffers and the caller's output
//! vector, so it does not allocate once warmed up (CLAUDE.md rule 2;
//! enforced by `tests/plan_alloc.rs`).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::graph::{Network, NodeId};
use crate::registry::Registry;

/// Weight of the newest sample in the moving average.
const EMA_ALPHA: f32 = 0.1;

/// Cook-time metrics for one node.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CookStat {
    pub cooks: u64,
    pub last_us: f32,
    /// Exponential moving average of cook time.
    pub avg_us: f32,
    pub max_us: f32,
}

/// Errors from planning a cook.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("requested node {0} does not exist")]
    NoNode(NodeId),
    #[error("dependency cycle through node {0}")]
    Cycle(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    InProgress,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Visit,
    /// An expression dependency: if it would close a cycle, it is skipped
    /// (the expression reads the previous frame's value).
    Soft,
    Expand,
}

/// What a node's parameter expressions add to the network (PRD A22).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extra {
    /// Nodes the expressions read: cooked first when possible.
    pub deps: Vec<NodeId>,
    /// An expression reads `time` or `frame`, or runs in Python.
    pub time_dependent: bool,
}

#[derive(Debug, Default)]
pub struct Scheduler {
    dirty: HashSet<NodeId>,
    stats: HashMap<NodeId, CookStat>,
    extra: HashMap<NodeId, Extra>,
    // Scratch buffers reused by `plan`.
    marks: HashMap<NodeId, Mark>,
    included: HashSet<NodeId>,
    /// Planned nodes that will be planned next frame too (see
    /// [`Scheduler::continuous`]).
    continuous: HashSet<NodeId>,
    stack: Vec<(NodeId, Step)>,
    /// Per node, a bit per input (schema order) it will not read this frame.
    idle: HashMap<NodeId, u64>,
    /// Scratch for `mark_dirty`.
    queue: Vec<NodeId>,
}

impl Scheduler {
    /// Marks `nodes` and everything downstream of them dirty, including
    /// nodes whose expressions read them.
    pub fn mark_dirty(&mut self, net: &Network, nodes: impl IntoIterator<Item = NodeId>) {
        let mut queue = std::mem::take(&mut self.queue);
        queue.clear();
        queue.extend(nodes);
        while let Some(n) = queue.pop() {
            if self.dirty.insert(n) {
                queue.extend(net.downstream_iter(n));
                queue.extend(
                    self.extra
                        .iter()
                        .filter(|(_, e)| e.deps.contains(&n))
                        .map(|(id, _)| *id),
                );
            }
        }
        self.queue = queue;
    }

    /// [`Self::mark_dirty`] for one node (a live value changing every
    /// frame). Allocates nothing once the dirty set and queue have grown.
    pub fn mark_dirty_one(&mut self, net: &Network, node: NodeId) {
        self.mark_dirty(net, std::iter::once(node));
    }

    /// Replaces the expression dependencies of every node.
    pub fn set_extra(&mut self, extra: HashMap<NodeId, Extra>) {
        self.extra = extra;
    }

    /// Drops state for a removed node.
    pub fn forget(&mut self, node: NodeId) {
        self.dirty.remove(&node);
        self.stats.remove(&node);
        self.idle.remove(&node);
    }

    /// Declares the inputs `node` will not read in the next plans: a bit per
    /// input in schema order (inputs past 64 are always read). Their sources
    /// are not cooked on its account (a Stage layer cooks only the clips it
    /// shows). Zero clears it.
    pub fn set_idle_inputs(&mut self, node: NodeId, mask: u64) {
        if mask == 0 {
            self.idle.remove(&node);
        } else {
            self.idle.insert(node, mask);
        }
    }

    pub fn is_dirty(&self, node: NodeId) -> bool {
        self.dirty.contains(&node) || !self.stats.contains_key(&node)
    }

    pub fn stat(&self, node: NodeId) -> Option<&CookStat> {
        self.stats.get(&node)
    }

    /// Whether the last [`Scheduler::plan`] included `node` for a reason that
    /// holds next frame too: it is time-dependent (or its expressions are),
    /// it has a wired feedback input, or it is downstream of such a node.
    /// A node planned only because an edit dirtied it is not continuous. The
    /// engine hands a GEO value to its consumer in place only when the source
    /// cooks again next frame anyway (design: geometry-attributes.md §3.2).
    pub fn continuous(&self, node: NodeId) -> bool {
        self.continuous.contains(&node)
    }

    /// Records that `node` cooked in `took`: clears its dirty flag and
    /// updates its metrics.
    pub fn record(&mut self, node: NodeId, took: Duration) {
        self.dirty.remove(&node);
        let us = took.as_secs_f32() * 1e6;
        let stat = self.stats.entry(node).or_default();
        stat.avg_us = if stat.cooks == 0 {
            us
        } else {
            stat.avg_us + EMA_ALPHA * (us - stat.avg_us)
        };
        stat.cooks += 1;
        stat.last_us = us;
        stat.max_us = stat.max_us.max(us);
    }

    /// Writes into `out`, in dependency order, the nodes that must cook this
    /// frame to produce `requested`.
    pub fn plan(
        &mut self,
        net: &Network,
        registry: &Registry,
        requested: &[NodeId],
        out: &mut Vec<NodeId>,
    ) -> Result<(), PlanError> {
        out.clear();
        self.marks.clear();
        self.included.clear();
        self.continuous.clear();
        self.stack.clear();
        for &r in requested.iter().rev() {
            if net.node(r).is_none() {
                return Err(PlanError::NoNode(r));
            }
            self.stack.push((r, Step::Visit));
        }

        while let Some((n, step)) = self.stack.pop() {
            let node = net.node(n).ok_or(PlanError::NoNode(n))?;
            let schema = registry.get(&node.type_id);
            let is_feedback = |input: &str| {
                schema
                    .and_then(|s| s.inputs.iter().find(|i| i.name == input))
                    .is_some_and(|i| i.feedback)
            };
            let idle_mask = self.idle.get(&n).copied().unwrap_or(0);
            let is_idle = |input: &str| {
                idle_mask != 0
                    && schema
                        .and_then(|s| s.inputs.iter().position(|i| i.name == input))
                        .is_some_and(|i| i < 64 && idle_mask & (1 << i) != 0)
            };

            let extra = self.extra.get(&n);
            if step == Step::Expand {
                let mut continuous = schema.is_some_and(|s| s.time_dependent)
                    || extra.is_some_and(|e| e.time_dependent);
                let mut needs =
                    continuous || self.dirty.contains(&n) || !self.stats.contains_key(&n);
                for (input, src) in net.inputs_of(n) {
                    if is_idle(input) {
                        continue;
                    }
                    if is_feedback(input) {
                        needs = true;
                        continuous = true;
                    } else if self.included.contains(&src.node) {
                        needs = true;
                        continuous |= self.continuous.contains(&src.node);
                    }
                }
                for b in net.bindings_of(n) {
                    if self.included.contains(&b.node) {
                        needs = true;
                        continuous |= self.continuous.contains(&b.node);
                    }
                }
                // A dependency still in progress closes a loop through
                // expressions: whether it cooks is not known yet, so cook.
                if let Some(e) = extra {
                    for d in &e.deps {
                        if self.marks.get(d) == Some(&Mark::InProgress) {
                            needs = true;
                            continuous = true;
                        } else if self.included.contains(d) {
                            needs = true;
                            continuous |= self.continuous.contains(d);
                        }
                    }
                }
                if needs {
                    self.included.insert(n);
                    out.push(n);
                    if continuous {
                        self.continuous.insert(n);
                    }
                }
                self.marks.insert(n, Mark::Done);
                continue;
            }

            match self.marks.get(&n) {
                Some(Mark::Done) => continue,
                Some(Mark::InProgress) if step == Step::Soft => continue,
                Some(Mark::InProgress) => return Err(PlanError::Cycle(n)),
                None => {}
            }
            self.marks.insert(n, Mark::InProgress);
            self.stack.push((n, Step::Expand));
            // Push inputs in reverse so they are visited in input-name order.
            let first = self.stack.len();
            for (input, src) in net.inputs_of(n) {
                if !is_feedback(input)
                    && !is_idle(input)
                    && self.marks.get(&src.node) != Some(&Mark::Done)
                {
                    self.stack.push((src.node, Step::Visit));
                }
            }
            // Channel bindings are dependencies too; dangling ones are inert.
            for b in net.bindings_of(n) {
                if net.node(b.node).is_some() && self.marks.get(&b.node) != Some(&Mark::Done) {
                    self.stack.push((b.node, Step::Visit));
                }
            }
            if let Some(e) = extra {
                for &d in &e.deps {
                    if d != n && net.node(d).is_some() && !self.marks.contains_key(&d) {
                        self.stack.push((d, Step::Soft));
                    }
                }
            }
            self.stack[first..].reverse();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::tests::{add, connect, test_registry};
    use crate::graph::{Edit, Param};
    use serde_json::json;

    fn cook_all(s: &mut Scheduler, plan: &[NodeId]) {
        for &n in plan {
            s.record(n, Duration::from_micros(10));
        }
    }

    #[test]
    fn plans_in_dependency_order_and_caches() {
        let reg = test_registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.gen");
        let b = add(&mut net, &reg, "img.gen");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, a, mix, "a");
        connect(&mut net, &reg, b, mix, "b");

        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        s.plan(&net, &reg, &[mix], &mut plan).unwrap();
        assert_eq!(plan, [a, b, mix], "never-cooked nodes cook, inputs first");
        cook_all(&mut s, &plan);

        s.plan(&net, &reg, &[mix], &mut plan).unwrap();
        assert!(plan.is_empty(), "nothing changed, everything cached");

        // Changing `b` recooks b and mix but not a.
        let applied = net
            .apply(
                &reg,
                Edit::SetParam {
                    node: b,
                    param: "gain".into(),
                    value: Some(Param::constant(json!(2.0))),
                },
            )
            .unwrap();
        s.mark_dirty(&net, applied.dirty);
        s.plan(&net, &reg, &[mix], &mut plan).unwrap();
        assert_eq!(plan, [b, mix]);
    }

    #[test]
    fn time_dependent_nodes_cook_every_frame_with_their_consumers() {
        let reg = test_registry();
        let mut net = Network::default();
        let lfo = add(&mut net, &reg, "sig.lfo");
        let g = add(&mut net, &reg, "img.gen");
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        s.plan(&net, &reg, &[lfo, g], &mut plan).unwrap();
        cook_all(&mut s, &plan);
        for _ in 0..3 {
            s.plan(&net, &reg, &[lfo, g], &mut plan).unwrap();
            assert_eq!(plan, [lfo]);
            cook_all(&mut s, &plan);
        }
    }

    #[test]
    fn expression_dependencies_order_and_dirty_but_never_fail() {
        let reg = test_registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.gen");
        let b = add(&mut net, &reg, "img.gen");
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        // b's expression reads a: a cooks first, even if only b is asked for.
        s.set_extra(HashMap::from([(
            b,
            Extra {
                deps: vec![a],
                time_dependent: false,
            },
        )]));
        s.plan(&net, &reg, &[b], &mut plan).unwrap();
        assert_eq!(plan, [a, b]);
        cook_all(&mut s, &plan);
        // Dirtying a dirties b through the expression.
        s.mark_dirty(&net, [a]);
        s.plan(&net, &reg, &[b], &mut plan).unwrap();
        assert_eq!(plan, [a, b]);
        cook_all(&mut s, &plan);
        // A loop through expressions reads last frame's value: no error.
        s.set_extra(HashMap::from([
            (
                a,
                Extra {
                    deps: vec![b],
                    time_dependent: true,
                },
            ),
            (
                b,
                Extra {
                    deps: vec![a],
                    time_dependent: false,
                },
            ),
        ]));
        s.plan(&net, &reg, &[a, b], &mut plan).unwrap();
        assert_eq!(plan.len(), 2, "time-dependent a and its reader b: {plan:?}");
        cook_all(&mut s, &plan);
        s.plan(&net, &reg, &[a, b], &mut plan).unwrap();
        assert_eq!(plan.len(), 2, "a cooks every frame, b follows");
    }

    #[test]
    fn unrequested_branches_do_not_cook() {
        let reg = test_registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.gen");
        let other = add(&mut net, &reg, "img.gen");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, a, mix, "a");
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        s.plan(&net, &reg, &[mix], &mut plan).unwrap();
        assert!(!plan.contains(&other), "lazy: only what is pulled cooks");
    }

    #[test]
    fn feedback_loops_cook_every_frame_without_cycling() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let fb = add(&mut net, &reg, "img.feedback");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, g, fb, "in");
        connect(&mut net, &reg, fb, mix, "a");
        connect(&mut net, &reg, mix, fb, "prev");
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        s.plan(&net, &reg, &[mix], &mut plan).unwrap();
        assert_eq!(plan, [g, fb, mix]);
        cook_all(&mut s, &plan);
        s.plan(&net, &reg, &[mix], &mut plan).unwrap();
        assert_eq!(
            plan,
            [fb, mix],
            "the loop advances each frame; the static source stays cached"
        );
    }

    #[test]
    fn continuous_nodes_are_those_that_cook_again_next_frame() {
        let reg = test_registry();
        let mut net = Network::default();
        let lfo = add(&mut net, &reg, "sig.lfo");
        let a = add(&mut net, &reg, "img.gen");
        let b = add(&mut net, &reg, "img.mix");
        let c = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, a, b, "a");
        connect(&mut net, &reg, b, c, "a");
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        // First cook: everything is planned, nothing is continuous.
        s.plan(&net, &reg, &[c, lfo], &mut plan).unwrap();
        assert_eq!(plan.len(), 4);
        assert!(plan.iter().all(|&n| !s.continuous(n) || n == lfo));
        assert!(s.continuous(lfo), "time-dependent");
        cook_all(&mut s, &plan);
        // An edit of `a` plans a, b, c once: not continuous.
        s.mark_dirty(&net, [a]);
        s.plan(&net, &reg, &[c], &mut plan).unwrap();
        assert_eq!(plan, [a, b, c]);
        assert!(!s.continuous(a) && !s.continuous(b) && !s.continuous(c));
        cook_all(&mut s, &plan);
        // Driving b from the LFO makes b and c continuous, not a.
        net.apply(
            &reg,
            Edit::SetParam {
                node: b,
                param: "gain".into(),
                value: Some(Param::bound(json!(1.0), lfo, "")),
            },
        )
        .unwrap();
        s.mark_dirty(&net, [b]);
        s.plan(&net, &reg, &[c], &mut plan).unwrap();
        cook_all(&mut s, &plan);
        s.plan(&net, &reg, &[c], &mut plan).unwrap();
        assert_eq!(plan, [lfo, b, c]);
        assert!(s.continuous(b) && s.continuous(c) && !s.continuous(a));
    }

    #[test]
    fn dirty_propagates_downstream() {
        let reg = test_registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.gen");
        let m1 = add(&mut net, &reg, "img.mix");
        let m2 = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, a, m1, "a");
        connect(&mut net, &reg, m1, m2, "a");
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        s.plan(&net, &reg, &[m2], &mut plan).unwrap();
        cook_all(&mut s, &plan);
        s.mark_dirty(&net, [a]);
        assert!(s.is_dirty(m2));
        s.plan(&net, &reg, &[m2], &mut plan).unwrap();
        assert_eq!(plan, [a, m1, m2]);
    }

    #[test]
    fn bound_parameters_follow_time_dependent_channels() {
        let reg = test_registry();
        let mut net = Network::default();
        let lfo = add(&mut net, &reg, "sig.lfo");
        let g = add(&mut net, &reg, "img.gen");
        net.apply(
            &reg,
            Edit::SetParam {
                node: g,
                param: "gain".into(),
                value: Some(Param::bound(json!(1.0), lfo, "")),
            },
        )
        .unwrap();
        let mut s = Scheduler::default();
        let mut plan = Vec::new();
        s.plan(&net, &reg, &[g], &mut plan).unwrap();
        assert_eq!(plan, [lfo, g], "the channel source cooks first");
        cook_all(&mut s, &plan);
        s.plan(&net, &reg, &[g], &mut plan).unwrap();
        assert_eq!(
            plan,
            [lfo, g],
            "a time-dependent channel re-cooks its consumer every frame"
        );
    }

    #[test]
    fn metrics_track_cook_time() {
        let mut s = Scheduler::default();
        let n = NodeId(7);
        s.record(n, Duration::from_micros(100));
        s.record(n, Duration::from_micros(200));
        let st = s.stat(n).unwrap();
        assert_eq!(st.cooks, 2);
        assert_eq!(st.last_us, 200.0);
        assert_eq!(st.max_us, 200.0);
        assert!((st.avg_us - 110.0).abs() < 0.01);
    }

    #[test]
    fn missing_request_is_an_error() {
        let reg = test_registry();
        let net = Network::default();
        let mut s = Scheduler::default();
        assert_eq!(
            s.plan(&net, &reg, &[NodeId(3)], &mut Vec::new()),
            Err(PlanError::NoNode(NodeId(3)))
        );
    }
}
