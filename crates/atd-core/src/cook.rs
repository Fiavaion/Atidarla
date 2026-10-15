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

#[derive(Debug, Default)]
pub struct Scheduler {
    dirty: HashSet<NodeId>,
    stats: HashMap<NodeId, CookStat>,
    // Scratch buffers reused by `plan`.
    marks: HashMap<NodeId, Mark>,
    included: HashSet<NodeId>,
    stack: Vec<(NodeId, bool)>,
}

impl Scheduler {
    /// Marks `nodes` and everything downstream of them dirty.
    pub fn mark_dirty(&mut self, net: &Network, nodes: impl IntoIterator<Item = NodeId>) {
        let mut queue: Vec<NodeId> = nodes.into_iter().collect();
        while let Some(n) = queue.pop() {
            if self.dirty.insert(n) {
                queue.extend(net.downstream_of(n));
            }
        }
    }

    /// Drops state for a removed node.
    pub fn forget(&mut self, node: NodeId) {
        self.dirty.remove(&node);
        self.stats.remove(&node);
    }

    pub fn is_dirty(&self, node: NodeId) -> bool {
        self.dirty.contains(&node) || !self.stats.contains_key(&node)
    }

    pub fn stat(&self, node: NodeId) -> Option<&CookStat> {
        self.stats.get(&node)
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
        self.stack.clear();
        for &r in requested.iter().rev() {
            if net.node(r).is_none() {
                return Err(PlanError::NoNode(r));
            }
            self.stack.push((r, false));
        }

        while let Some((n, expanded)) = self.stack.pop() {
            let node = net.node(n).ok_or(PlanError::NoNode(n))?;
            let schema = registry.get(&node.type_id);
            let is_feedback = |input: &str| {
                schema
                    .and_then(|s| s.inputs.iter().find(|i| i.name == input))
                    .is_some_and(|i| i.feedback)
            };

            if expanded {
                let mut needs = self.dirty.contains(&n)
                    || !self.stats.contains_key(&n)
                    || schema.is_some_and(|s| s.time_dependent);
                for (input, src) in net.inputs_of(n) {
                    if is_feedback(input) || self.included.contains(&src.node) {
                        needs = true;
                    }
                }
                if needs {
                    self.included.insert(n);
                    out.push(n);
                }
                self.marks.insert(n, Mark::Done);
                continue;
            }

            match self.marks.get(&n) {
                Some(Mark::Done) => continue,
                Some(Mark::InProgress) => return Err(PlanError::Cycle(n)),
                None => {}
            }
            self.marks.insert(n, Mark::InProgress);
            self.stack.push((n, true));
            // Push inputs in reverse so they are visited in input-name order.
            let first = self.stack.len();
            for (input, src) in net.inputs_of(n) {
                if !is_feedback(input) && self.marks.get(&src.node) != Some(&Mark::Done) {
                    self.stack.push((src.node, false));
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
