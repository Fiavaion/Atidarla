//! Fragments: a piece of a network (nodes and the wires between them) for
//! copy/paste, duplicate and, later, agent-proposed patches (FR-P1, FR-G2).
//!
//! A fragment serialises to JSON with a marker key, so the system clipboard
//! can carry it between windows and projects. [`Fragment::paste`] turns it
//! into ordinary [`Edit`]s for one undoable transaction:
//!
//! - nodes get fresh IDs and unique names;
//! - wires inside the fragment are remapped;
//! - channel bindings to nodes inside the fragment follow the copies, and
//!   bindings to outside nodes are kept when that node exists in the target
//!   network, otherwise dropped.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::graph::{Edit, Endpoint, Network, Node, NodeId, Wire};

/// Marker value written under [`MARKER`] so pasting arbitrary JSON fails fast.
const FORMAT: &str = "atidarla.fragment/1";
const MARKER: &str = "format";

/// Copied nodes and their internal wires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fragment {
    format: String,
    pub nodes: Vec<Node>,
    pub wires: Vec<Wire>,
}

impl Fragment {
    /// Copies `ids` (missing IDs are ignored) and the wires between them.
    pub fn copy(net: &Network, ids: &BTreeSet<NodeId>) -> Self {
        let nodes: Vec<Node> = ids.iter().filter_map(|id| net.node(*id).cloned()).collect();
        let wires = net
            .wires()
            .filter(|w| ids.contains(&w.from.node) && ids.contains(&w.to.node))
            .collect();
        Self {
            format: FORMAT.into(),
            nodes,
            wires,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("fragments serialise")
    }

    /// Parses clipboard text; `None` unless it is a fragment.
    pub fn from_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
        if value.get(MARKER)?.as_str()? != FORMAT {
            return None;
        }
        serde_json::from_value(value).ok()
    }

    /// Top-left corner of the copied nodes.
    pub fn origin(&self) -> [f32; 2] {
        self.nodes.iter().fold([f32::INFINITY; 2], |o, n| {
            [o[0].min(n.pos[0]), o[1].min(n.pos[1])]
        })
    }

    /// Edits that add a copy of the fragment to `net`, moved by `offset`.
    /// `alloc` hands out fresh node IDs. Returns the edits and the new IDs
    /// in the fragment's node order.
    pub fn paste(
        &self,
        net: &Network,
        mut alloc: impl FnMut() -> NodeId,
        offset: [f32; 2],
    ) -> (Vec<Edit>, Vec<NodeId>) {
        let map: BTreeMap<NodeId, NodeId> = self.nodes.iter().map(|n| (n.id, alloc())).collect();
        let mut taken: HashSet<String> = HashSet::new();
        let mut edits = Vec::new();
        let mut bound = Vec::new();
        let mut new_ids = Vec::new();
        for node in &self.nodes {
            let id = map[&node.id];
            new_ids.push(id);
            let mut copy = node.clone();
            copy.id = id;
            copy.name = fresh_name(net, &node.name, &mut taken);
            copy.pos = [node.pos[0] + offset[0], node.pos[1] + offset[1]];
            // Bindings are applied after every node exists, so they are
            // validated (including cycle checks) like any other edit.
            for (name, param) in &mut copy.params {
                if let Some(mut bind) = param.bind.take() {
                    match map.get(&bind.node) {
                        Some(new) => bind.node = *new,
                        None if net.node(bind.node).is_some() => {}
                        None => continue,
                    }
                    let mut p = param.clone();
                    p.bind = Some(bind);
                    bound.push(Edit::SetParam {
                        node: id,
                        param: name.clone(),
                        value: Some(p),
                    });
                }
            }
            edits.push(Edit::AddNode { node: copy });
        }
        for wire in &self.wires {
            edits.push(Edit::Connect {
                wire: Wire::new(
                    Endpoint::new(map[&wire.from.node], wire.from.socket.clone()),
                    Endpoint::new(map[&wire.to.node], wire.to.socket.clone()),
                ),
            });
        }
        edits.extend(bound);
        (edits, new_ids)
    }
}

/// `noise3` → `noise4` (or the next free number), unique in `net` and among
/// names already handed out in this paste.
fn fresh_name(net: &Network, name: &str, taken: &mut HashSet<String>) -> String {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = if base.is_empty() { "node" } else { base };
    let name = (1..)
        .map(|n| format!("{base}{n}"))
        .find(|n| !taken.contains(n) && net.node_by_name(n).is_none())
        .expect("an unused name exists");
    taken.insert(name.clone());
    name
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::graph::{ChannelRef, Param};
    use crate::history::transact;
    use crate::registry::{
        OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType,
    };

    fn registry() -> Registry {
        let socket = |name: &str, wire| SocketSchema {
            name: name.into(),
            wire,
            optional: true,
            feedback: false,
        };
        let mut reg = Registry::default();
        for (id, family, wire) in [
            ("img.a", OpFamily::Img, WireType::Img),
            ("sig.s", OpFamily::Sig, WireType::Sig),
        ] {
            reg.register(OperatorSchema {
                type_id: id.into(),
                family,
                label: id.into(),
                summary: String::new(),
                version: 1,
                params: vec![ParamSchema::float("x", "X", 0.0, None, None)],
                inputs: vec![socket("in", wire)],
                outputs: vec![socket("out", wire)],
                capabilities: vec![],
                cook_budget_us: 1,
                time_dependent: false,
                sink: false,
            })
            .unwrap();
        }
        reg
    }

    fn add(net: &mut Network, reg: &Registry, ty: &str, name: &str) -> NodeId {
        let id = net.alloc_id();
        let mut node = Node::new(id, ty, name);
        node.pos = [id.0 as f32 * 10.0, 5.0];
        net.apply(reg, Edit::AddNode { node }).unwrap();
        id
    }

    fn connect(net: &mut Network, reg: &Registry, a: NodeId, b: NodeId) {
        let wire = Wire::new(Endpoint::new(a, "out"), Endpoint::new(b, "in"));
        net.apply(reg, Edit::Connect { wire }).unwrap();
    }

    fn bind(net: &mut Network, reg: &Registry, node: NodeId, src: NodeId) {
        let mut p = Param::constant(json!(0.0));
        p.bind = Some(ChannelRef {
            node: src,
            channel: "c".into(),
        });
        net.apply(
            reg,
            Edit::SetParam {
                node,
                param: "x".into(),
                value: Some(p),
            },
        )
        .unwrap();
    }

    #[test]
    fn copy_paste_remaps_ids_names_wires_and_bindings() {
        let reg = registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.a", "a1");
        let b = add(&mut net, &reg, "img.a", "a2");
        let outside = add(&mut net, &reg, "img.a", "src1");
        let lfo = add(&mut net, &reg, "sig.s", "lfo1");
        let ext = add(&mut net, &reg, "sig.s", "ext1");
        connect(&mut net, &reg, outside, a); // into the selection: not copied
        connect(&mut net, &reg, a, b);
        bind(&mut net, &reg, a, lfo); // inside: follows the copy
        bind(&mut net, &reg, b, ext); // outside: kept

        let frag = Fragment::copy(&net, &BTreeSet::from([a, b, lfo]));
        assert_eq!(frag.nodes.len(), 3);
        assert_eq!(frag.wires.len(), 1);
        let text = frag.to_json();
        let back = Fragment::from_json(&text).expect("round trip");
        assert_eq!(back, frag);

        let mut next = 100;
        let (edits, ids) = back.paste(
            &net,
            || {
                next += 1;
                NodeId(next)
            },
            [30.0, 40.0],
        );
        transact(&mut net, &reg, edits).unwrap();
        assert_eq!(ids, [NodeId(101), NodeId(102), NodeId(103)]);
        let names: Vec<_> = ids
            .iter()
            .map(|id| net.node(*id).unwrap().name.clone())
            .collect();
        assert_eq!(names, ["a3", "a4", "lfo2"]);
        assert_eq!(
            net.node(ids[0]).unwrap().pos,
            [net.node(a).unwrap().pos[0] + 30.0, 45.0]
        );
        let wires: Vec<Wire> = net.wires().filter(|w| ids.contains(&w.to.node)).collect();
        assert_eq!(
            wires,
            [Wire::new(
                Endpoint::new(ids[0], "out"),
                Endpoint::new(ids[1], "in")
            )]
        );
        let bind_of = |id: NodeId| net.node(id).unwrap().params["x"].bind.clone().unwrap().node;
        assert_eq!(bind_of(ids[0]), ids[2], "inside binding follows the copy");
        assert_eq!(bind_of(ids[1]), ext, "outside binding is kept");
    }

    #[test]
    fn bindings_to_missing_nodes_are_dropped_and_junk_is_rejected() {
        let reg = registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.a", "a1");
        let ext = add(&mut net, &reg, "sig.s", "ext1");
        bind(&mut net, &reg, a, ext);
        let frag = Fragment::copy(&net, &BTreeSet::from([a]));

        let mut other = Network::default();
        let mut next = 0;
        let (edits, ids) = frag.paste(
            &other,
            || {
                next += 1;
                NodeId(next)
            },
            [0.0, 0.0],
        );
        transact(&mut other, &reg, edits).unwrap();
        let node = other.node(ids[0]).unwrap();
        assert_eq!(node.name, "a1");
        assert!(node.params.get("x").is_none_or(|p| p.bind.is_none()));

        assert!(Fragment::from_json("{\"nodes\": []}").is_none());
        assert!(Fragment::from_json("not json").is_none());
        assert!(Fragment::copy(&net, &BTreeSet::new()).is_empty());
    }
}
