//! Transactions and undo/redo over [`Edit`]s.
//!
//! A transaction applies a batch of edits atomically: if any edit fails, the
//! ones already applied are rolled back and the network is unchanged. Each
//! committed transaction is one undo step (accepted agent diffs use this,
//! FR-G2). Undoing applies the stored inverse edits as a transaction, and
//! the inverse of *that* becomes the redo step, so undo and redo can never
//! drift apart.

use crate::graph::{Edit, GraphError, Network, NodeId};
use crate::registry::Registry;

/// One undoable step.
#[derive(Debug, Clone, PartialEq)]
struct Step {
    label: String,
    /// Edits that revert the step, in application order.
    edits: Vec<Edit>,
    /// Steps with the same key merge while unsealed (a slider drag).
    merge: Option<String>,
}

/// Outcome of a committed transaction, undo or redo.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Committed {
    /// Nodes whose cook results changed (before downstream propagation).
    pub dirty: Vec<NodeId>,
}

/// Applies `edits` atomically. On success returns the inverse edits (in
/// application order) and the dirty nodes; on failure the network is
/// restored and the first error returned.
pub fn transact(
    net: &mut Network,
    registry: &Registry,
    edits: impl IntoIterator<Item = Edit>,
) -> Result<(Vec<Edit>, Vec<NodeId>), GraphError> {
    let mut inverses: Vec<Vec<Edit>> = Vec::new();
    let mut dirty = Vec::new();
    for edit in edits {
        match net.apply(registry, edit) {
            Ok(applied) => {
                inverses.push(applied.inverse);
                dirty.extend(applied.dirty);
            }
            Err(e) => {
                for inverse in inverses.into_iter().rev() {
                    for edit in inverse {
                        net.apply(registry, edit)
                            .expect("rolling back a just-applied edit cannot fail");
                    }
                }
                return Err(e);
            }
        }
    }
    let undo = inverses.into_iter().rev().flatten().collect();
    dirty.sort_unstable();
    dirty.dedup();
    Ok((undo, dirty))
}

/// Undo/redo stacks for one network.
#[derive(Debug, Clone)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::with_limit(1000)
    }
}

impl History {
    /// Keeps at most `limit` undo steps (oldest dropped first).
    pub fn with_limit(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Applies `edits` as one undo step labelled `label`. A new step clears
    /// the redo stack.
    pub fn commit(
        &mut self,
        net: &mut Network,
        registry: &Registry,
        label: impl Into<String>,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<Committed, GraphError> {
        self.push(net, registry, label.into(), None, edits)
    }

    /// Like [`Self::commit`], but merges into the previous step when that
    /// step has the same `key` and has not been sealed, so a continuous
    /// gesture (dragging a slider) is one undo step. The merged step keeps
    /// the inverse captured before the gesture began.
    pub fn commit_coalesced(
        &mut self,
        net: &mut Network,
        registry: &Registry,
        label: impl Into<String>,
        key: impl Into<String>,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<Committed, GraphError> {
        let key = key.into();
        let merging = self.redo.is_empty()
            && self
                .undo
                .last()
                .is_some_and(|s| s.merge.as_deref() == Some(key.as_str()));
        if merging {
            let (_, dirty) = transact(net, registry, edits)?;
            return Ok(Committed { dirty });
        }
        self.push(net, registry, label.into(), Some(key), edits)
    }

    /// Ends any ongoing coalescing, so the next edit starts a new step.
    pub fn seal(&mut self) {
        if let Some(step) = self.undo.last_mut() {
            step.merge = None;
        }
    }

    fn push(
        &mut self,
        net: &mut Network,
        registry: &Registry,
        label: String,
        merge: Option<String>,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<Committed, GraphError> {
        let (inverse, dirty) = transact(net, registry, edits)?;
        if !inverse.is_empty() {
            self.undo.push(Step {
                label,
                edits: inverse,
                merge,
            });
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        Ok(Committed { dirty })
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the step [`Self::undo`] would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Reverts the last step. `Ok(None)` when there is nothing to undo.
    pub fn undo(
        &mut self,
        net: &mut Network,
        registry: &Registry,
    ) -> Result<Option<Committed>, GraphError> {
        Self::swap(&mut self.undo, &mut self.redo, net, registry)
    }

    /// Re-applies the last undone step.
    pub fn redo(
        &mut self,
        net: &mut Network,
        registry: &Registry,
    ) -> Result<Option<Committed>, GraphError> {
        Self::swap(&mut self.redo, &mut self.undo, net, registry)
    }

    fn swap(
        from: &mut Vec<Step>,
        to: &mut Vec<Step>,
        net: &mut Network,
        registry: &Registry,
    ) -> Result<Option<Committed>, GraphError> {
        let Some(step) = from.pop() else {
            return Ok(None);
        };
        match transact(net, registry, step.edits.clone()) {
            Ok((inverse, dirty)) => {
                to.push(Step {
                    label: step.label,
                    edits: inverse,
                    merge: None,
                });
                Ok(Some(Committed { dirty }))
            }
            Err(e) => {
                // Keep the step so the user can retry after fixing the cause.
                from.push(step);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::tests::{add, test_registry};
    use crate::graph::{Endpoint, Node, Param, Wire};
    use serde_json::json;

    #[test]
    fn transaction_is_atomic() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let before = net.clone();
        let id = net.alloc_id();
        let edits = vec![
            Edit::AddNode {
                node: Node::new(id, "img.mix", "mix1"),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(g, "out"), Endpoint::new(id, "a")),
            },
            Edit::SetParam {
                node: g,
                param: "gain".into(),
                value: Some(Param::constant(json!(-1))),
            },
        ];
        assert!(matches!(
            transact(&mut net, &reg, edits),
            Err(GraphError::InvalidValue(_))
        ));
        // Only the ID counter moved (IDs are never reused).
        assert_eq!(net.nodes().count(), before.nodes().count());
        assert_eq!(net.wires().count(), 0);
    }

    #[test]
    fn undo_redo_round_trip() {
        let reg = test_registry();
        let mut net = Network::default();
        let mut history = History::default();
        let g = net.alloc_id();
        let m = net.alloc_id();
        let empty = net.clone();

        history
            .commit(
                &mut net,
                &reg,
                "add gen and mix",
                vec![
                    Edit::AddNode {
                        node: Node::new(g, "img.gen", "gen1"),
                    },
                    Edit::AddNode {
                        node: Node::new(m, "img.mix", "mix1"),
                    },
                    Edit::Connect {
                        wire: Wire::new(Endpoint::new(g, "out"), Endpoint::new(m, "a")),
                    },
                ],
            )
            .unwrap();
        let built = net.clone();
        history
            .commit(
                &mut net,
                &reg,
                "gain",
                [Edit::SetParam {
                    node: m,
                    param: "gain".into(),
                    value: Some(Param::constant(json!(4.0))),
                }],
            )
            .unwrap();
        let tuned = net.clone();

        assert_eq!(history.undo_label(), Some("gain"));
        history.undo(&mut net, &reg).unwrap().unwrap();
        assert_eq!(net, built);
        history.undo(&mut net, &reg).unwrap().unwrap();
        assert_eq!(net, empty);
        assert_eq!(history.undo(&mut net, &reg).unwrap(), None);

        history.redo(&mut net, &reg).unwrap().unwrap();
        assert_eq!(net, built);
        history.redo(&mut net, &reg).unwrap().unwrap();
        assert_eq!(net, tuned);
        assert!(!history.can_redo());

        // A new step after an undo clears redo.
        history.undo(&mut net, &reg).unwrap();
        history
            .commit(
                &mut net,
                &reg,
                "move",
                [Edit::Move {
                    node: g,
                    pos: [5.0, 5.0],
                }],
            )
            .unwrap();
        assert!(!history.can_redo());
    }

    #[test]
    fn undo_reports_dirty_nodes() {
        let reg = test_registry();
        let mut net = Network::default();
        let mut history = History::default();
        let g = add(&mut net, &reg, "img.gen");
        let c = history
            .commit(
                &mut net,
                &reg,
                "gain",
                [Edit::SetParam {
                    node: g,
                    param: "gain".into(),
                    value: Some(Param::constant(json!(2.0))),
                }],
            )
            .unwrap();
        assert_eq!(c.dirty, vec![g]);
        assert_eq!(
            history.undo(&mut net, &reg).unwrap().unwrap().dirty,
            vec![g]
        );
    }

    #[test]
    fn coalesced_edits_are_one_step_until_sealed() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let mut history = History::default();
        let gain = |v: f64| Edit::SetParam {
            node: g,
            param: "gain".into(),
            value: Some(Param::constant(json!(v))),
        };
        for v in [2.0, 3.0, 4.0] {
            history
                .commit_coalesced(&mut net, &reg, "drag gain", "gain", [gain(v)])
                .unwrap();
        }
        history.seal();
        history
            .commit_coalesced(&mut net, &reg, "drag gain", "gain", [gain(5.0)])
            .unwrap();
        assert_eq!(net.param_value(&reg, g, "gain"), Some(&json!(5.0)));
        history.undo(&mut net, &reg).unwrap();
        assert_eq!(
            net.param_value(&reg, g, "gain"),
            Some(&json!(4.0)),
            "second gesture undone alone"
        );
        history.undo(&mut net, &reg).unwrap();
        assert_eq!(
            net.param_value(&reg, g, "gain"),
            Some(&json!(1.0)),
            "first gesture undone in one step"
        );
        assert!(!history.can_undo());
    }

    #[test]
    fn limit_drops_oldest() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let mut history = History::with_limit(2);
        for i in 0..5 {
            history
                .commit(
                    &mut net,
                    &reg,
                    format!("move {i}"),
                    [Edit::Move {
                        node: g,
                        pos: [i as f32, 0.0],
                    }],
                )
                .unwrap();
        }
        assert_eq!(history.undo_label(), Some("move 4"));
        history.undo(&mut net, &reg).unwrap();
        history.undo(&mut net, &reg).unwrap();
        assert!(!history.can_undo());
        assert_eq!(net.node(g).unwrap().pos, [2.0, 0.0]);
    }
}
