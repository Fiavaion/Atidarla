//! Graph store: one network of nodes and typed wires (PRD §5.2 `atd-core`).
//!
//! - **Stable IDs:** [`NodeId`]s come from a per-network counter and are never
//!   reused; files and wires reference IDs, people see names (`noise1`).
//! - **Deterministic order:** nodes and wires live in ordered maps, so
//!   iteration and serialisation are stable (CLAUDE.md rule 8).
//! - **Typed wires:** an output connects only to an input of the same
//!   [`WireType`]; an input holds at most one wire. Cycles are rejected
//!   unless they pass through a feedback input.
//! - **Edits:** every change is an [`Edit`] applied with [`Network::apply`],
//!   which validates it against the [`Registry`] and returns its exact
//!   inverse plus the nodes whose results it invalidates. Undo and
//!   transactions live in [`crate::history`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{OperatorSchema, Registry, SocketSchema, WireType};

/// Stable node identifier, unique within a network and never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "n{}", self.0)
    }
}

/// Stable comment-box identifier. Comments draw from the same counter as
/// nodes, so every ID in a network is unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommentId(pub u64);

impl fmt::Display for CommentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// A comment box on the Patch canvas (FR-P1): a note with an area.
/// Moving it in the editor also moves the nodes inside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub text: String,
    /// Top-left corner and size in canvas units.
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

/// A parameter override: a constant value, optionally driven by an expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub value: Value,
    /// Expression source (Python, M1); `value` holds its last result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    /// Channel export (FR-P2): the parameter follows a SIG channel; `value`
    /// is the fallback when the channel is unavailable. Resolved natively,
    /// never through Python, so it is safe on the cook path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<ChannelRef>,
}

impl Param {
    pub fn constant(value: Value) -> Self {
        Self {
            value,
            expr: None,
            bind: None,
        }
    }

    /// A parameter driven by `channel` of SIG node `node`, falling back to `value`.
    pub fn bound(value: Value, node: NodeId, channel: impl Into<String>) -> Self {
        Self {
            value,
            expr: None,
            bind: Some(ChannelRef {
                node,
                channel: channel.into(),
            }),
        }
    }
}

/// A reference to one channel of a SIG node's output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChannelRef {
    pub node: NodeId,
    /// Channel name; empty means the first channel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
}

/// One operator instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// Operator type, e.g. `img.noise`.
    pub type_id: String,
    /// Unique, human-facing name within the network, e.g. `noise1`.
    pub name: String,
    /// Position on the Patch canvas.
    pub pos: [f32; 2],
    /// Only parameters that differ from the schema default are stored, so
    /// project files and diffs show what the user actually changed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Param>,
}

impl Node {
    pub fn new(id: NodeId, type_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id,
            type_id: type_id.into(),
            name: name.into(),
            pos: [0.0, 0.0],
            params: BTreeMap::new(),
        }
    }
}

/// One end of a wire: a node and the name of one of its sockets.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub node: NodeId,
    pub socket: String,
}

impl Endpoint {
    pub fn new(node: NodeId, socket: impl Into<String>) -> Self {
        Self {
            node,
            socket: socket.into(),
        }
    }
}

/// A connection from an output to an input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Wire {
    pub from: Endpoint,
    pub to: Endpoint,
}

impl Wire {
    pub fn new(from: Endpoint, to: Endpoint) -> Self {
        Self { from, to }
    }
}

/// Errors from graph edits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("unknown operator type `{0}`")]
    UnknownType(String),
    #[error("no node {0}")]
    NoNode(NodeId),
    #[error("node {0} already exists")]
    DuplicateId(NodeId),
    #[error("no comment {0}")]
    NoComment(CommentId),
    #[error("ID {0} is already in use")]
    CommentIdInUse(CommentId),
    #[error("a node named `{0}` already exists")]
    DuplicateName(String),
    #[error("invalid node name `{0}`: use letters, digits and `_`, starting with a letter or `_`")]
    InvalidName(String),
    #[error("`{type_id}` has no parameter `{param}`")]
    NoParam { type_id: String, param: String },
    #[error("{0}")]
    InvalidValue(String),
    #[error("`{type_id}` has no {kind} socket `{socket}`")]
    NoSocket {
        type_id: String,
        kind: &'static str,
        socket: String,
    },
    #[error("cannot connect {from:?} output to {to:?} input")]
    WireTypeMismatch { from: WireType, to: WireType },
    #[error("connecting {0} would create a cycle (only feedback inputs may close a loop)")]
    Cycle(String),
    #[error("wire {0} does not exist")]
    NoWire(String),
    #[error("invalid channel binding: {0}")]
    InvalidBinding(String),
}

/// A single, invertible change to a network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum Edit {
    /// Adds a node (with no wires). Its ID must not exist yet.
    AddNode {
        node: Node,
    },
    /// Removes a node and every wire attached to it.
    RemoveNode {
        id: NodeId,
    },
    /// Re-inserts a removed node with its wires (inverse of `RemoveNode`).
    RestoreNode {
        node: Node,
        wires: Vec<Wire>,
    },
    /// Connects an output to an input, replacing any wire on that input.
    Connect {
        wire: Wire,
    },
    /// Removes an existing wire.
    Disconnect {
        wire: Wire,
    },
    /// Sets (`Some`) or resets to default (`None`) a parameter.
    SetParam {
        node: NodeId,
        param: String,
        value: Option<Param>,
    },
    Rename {
        node: NodeId,
        name: String,
    },
    Move {
        node: NodeId,
        pos: [f32; 2],
    },
    /// Adds a comment box. Its ID must not be in use.
    AddComment {
        comment: Comment,
    },
    RemoveComment {
        id: CommentId,
    },
    /// Replaces a comment's text, position and size.
    UpdateComment {
        comment: Comment,
    },
}

/// Result of applying one edit.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// Edits that undo this one, to be applied in order.
    pub inverse: Vec<Edit>,
    /// Nodes whose cook results this edit invalidates (before downstream
    /// propagation, which the scheduler does).
    pub dirty: Vec<NodeId>,
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A network of nodes and wires.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "NetworkRepr", into = "NetworkRepr")]
pub struct Network {
    nodes: BTreeMap<NodeId, Node>,
    /// Wires keyed by their input (each input has at most one wire), ordered
    /// by input node so a node's inputs are one range lookup.
    inputs: BTreeMap<Endpoint, Endpoint>,
    comments: BTreeMap<CommentId, Comment>,
    next_id: u64,
}

/// Serialised form: nodes, wires and comments as sorted lists, which read
/// and diff well in text files. `comments` is omitted when empty, so files
/// without comments are unchanged.
#[derive(Serialize, Deserialize)]
struct NetworkRepr {
    next_id: u64,
    nodes: Vec<Node>,
    wires: Vec<Wire>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<Comment>,
}

impl From<Network> for NetworkRepr {
    fn from(net: Network) -> Self {
        let wires = net.wires().collect();
        Self {
            next_id: net.next_id,
            nodes: net.nodes.into_values().collect(),
            wires,
            comments: net.comments.into_values().collect(),
        }
    }
}

impl TryFrom<NetworkRepr> for Network {
    type Error = String;

    /// Structural checks only; schema checks need a [`Registry`].
    fn try_from(repr: NetworkRepr) -> Result<Self, String> {
        let mut net = Network {
            next_id: repr.next_id,
            ..Default::default()
        };
        for node in repr.nodes {
            if node.id.0 >= net.next_id {
                return Err(format!(
                    "node {} is not below next_id {}",
                    node.id, net.next_id
                ));
            }
            if let Some(old) = net.nodes.insert(node.id, node) {
                return Err(format!("duplicate node {}", old.id));
            }
        }
        for wire in repr.wires {
            for end in [&wire.from, &wire.to] {
                if !net.nodes.contains_key(&end.node) {
                    return Err(format!("wire references missing node {}", end.node));
                }
            }
            if net.inputs.insert(wire.to.clone(), wire.from).is_some() {
                return Err(format!(
                    "input {}.{} has two wires",
                    wire.to.node, wire.to.socket
                ));
            }
        }
        for comment in repr.comments {
            let id = comment.id;
            if id.0 >= net.next_id || net.nodes.contains_key(&NodeId(id.0)) {
                return Err(format!("comment {id} is not a fresh ID below next_id"));
            }
            if net.comments.insert(id, comment).is_some() {
                return Err(format!("duplicate comment {id}"));
            }
        }
        Ok(net)
    }
}

impl Network {
    /// Reserves a fresh comment ID (from the node ID counter).
    pub fn alloc_comment_id(&mut self) -> CommentId {
        CommentId(self.alloc_id().0)
    }

    pub fn comment(&self, id: CommentId) -> Option<&Comment> {
        self.comments.get(&id)
    }

    /// Comments in ID order.
    pub fn comments(&self) -> impl Iterator<Item = &Comment> {
        self.comments.values()
    }

    /// Reserves a fresh node ID. IDs are never reused, even after removal.
    pub fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// `base` followed by the smallest number that makes it unique (`noise1`).
    pub fn unique_name(&self, base: &str) -> String {
        (1..)
            .map(|n| format!("{base}{n}"))
            .find(|name| self.node_by_name(name).is_none())
            .expect("an unused name exists")
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn node_by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.values().find(|n| n.name == name)
    }

    /// Nodes in ID order.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// All wires, ordered by input node and input name.
    pub fn wires(&self) -> impl Iterator<Item = Wire> + '_ {
        self.inputs
            .iter()
            .map(|(to, from)| Wire::new(from.clone(), to.clone()))
    }

    /// The wires into `node` as `(input name, source)`, in input-name order.
    /// Does not allocate.
    pub fn inputs_of(&self, node: NodeId) -> impl Iterator<Item = (&str, &Endpoint)> {
        let start = Endpoint {
            node,
            socket: String::new(),
        };
        self.inputs
            .range(start..)
            .take_while(move |(to, _)| to.node == node)
            .map(|(to, from)| (to.socket.as_str(), from))
    }

    /// Channel bindings on `node`'s parameters. Does not allocate.
    pub fn bindings_of(&self, node: NodeId) -> impl Iterator<Item = &ChannelRef> {
        self.nodes
            .get(&node)
            .into_iter()
            .flat_map(|n| n.params.values().filter_map(|p| p.bind.as_ref()))
    }

    /// Nodes that depend on `node` through a wire or a channel binding, in ID
    /// order, without duplicates.
    pub fn downstream_of(&self, node: NodeId) -> BTreeSet<NodeId> {
        let wired = self
            .inputs
            .iter()
            .filter(|(_, from)| from.node == node)
            .map(|(to, _)| to.node);
        let bound = self
            .nodes
            .values()
            .filter(|n| {
                n.params
                    .values()
                    .any(|p| p.bind.as_ref().is_some_and(|b| b.node == node))
            })
            .map(|n| n.id);
        wired.chain(bound).collect()
    }

    /// The parameter value of a node, falling back to the schema default.
    pub fn param_value<'a>(
        &'a self,
        registry: &'a Registry,
        node: NodeId,
        param: &str,
    ) -> Option<&'a Value> {
        let n = self.nodes.get(&node)?;
        if let Some(p) = n.params.get(param) {
            return Some(&p.value);
        }
        let schema = registry.get(&n.type_id)?;
        schema
            .params
            .iter()
            .find(|p| p.name == param)
            .map(|p| &p.default)
    }

    fn schema<'r>(
        &self,
        registry: &'r Registry,
        id: NodeId,
    ) -> Result<&'r OperatorSchema, GraphError> {
        let node = self.nodes.get(&id).ok_or(GraphError::NoNode(id))?;
        registry
            .get(&node.type_id)
            .ok_or_else(|| GraphError::UnknownType(node.type_id.clone()))
    }

    fn socket<'r>(
        schema: &'r OperatorSchema,
        output: bool,
        name: &str,
    ) -> Result<&'r SocketSchema, GraphError> {
        let list = if output {
            &schema.outputs
        } else {
            &schema.inputs
        };
        list.iter()
            .find(|s| s.name == name)
            .ok_or_else(|| GraphError::NoSocket {
                type_id: schema.type_id.clone(),
                kind: if output { "output" } else { "input" },
                socket: name.to_owned(),
            })
    }

    /// True if `to` is reachable from `from` following wires and channel
    /// bindings forward, ignoring wires into feedback inputs.
    fn reaches(&self, registry: &Registry, from: NodeId, to: NodeId) -> bool {
        let mut stack = vec![from];
        let mut seen = BTreeSet::new();
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if !seen.insert(n) {
                continue;
            }
            for (input, src) in &self.inputs {
                if src.node != n {
                    continue;
                }
                let is_feedback = self
                    .schema(registry, input.node)
                    .ok()
                    .and_then(|s| Self::socket(s, false, &input.socket).ok())
                    .is_some_and(|s| s.feedback);
                if !is_feedback {
                    stack.push(input.node);
                }
            }
            for other in self.nodes.values() {
                if other
                    .params
                    .values()
                    .any(|p| p.bind.as_ref().is_some_and(|b| b.node == n))
                {
                    stack.push(other.id);
                }
            }
        }
        false
    }

    fn validate_binding(
        &self,
        registry: &Registry,
        node: NodeId,
        param: &crate::registry::ParamSchema,
        bind: &ChannelRef,
    ) -> Result<(), GraphError> {
        use crate::registry::ParamKind;
        if !matches!(
            param.kind,
            ParamKind::Float { .. } | ParamKind::Int { .. } | ParamKind::Toggle
        ) {
            return Err(GraphError::InvalidBinding(format!(
                "parameter `{}` is not numeric and cannot follow a channel",
                param.name
            )));
        }
        let source = self.schema(registry, bind.node)?;
        if !source.outputs.iter().any(|o| o.wire == WireType::Sig) {
            return Err(GraphError::InvalidBinding(format!(
                "{} (`{}`) has no SIG output",
                bind.node, source.type_id
            )));
        }
        if bind.node == node || self.reaches(registry, node, bind.node) {
            return Err(GraphError::Cycle(format!("binding {} → {node}", bind.node)));
        }
        Ok(())
    }

    fn validate_node(&self, registry: &Registry, node: &Node) -> Result<(), GraphError> {
        let schema = registry
            .get(&node.type_id)
            .ok_or_else(|| GraphError::UnknownType(node.type_id.clone()))?;
        if !valid_name(&node.name) {
            return Err(GraphError::InvalidName(node.name.clone()));
        }
        if self
            .nodes
            .values()
            .any(|n| n.name == node.name && n.id != node.id)
        {
            return Err(GraphError::DuplicateName(node.name.clone()));
        }
        for (name, param) in &node.params {
            let ps = schema
                .params
                .iter()
                .find(|p| &p.name == name)
                .ok_or_else(|| GraphError::NoParam {
                    type_id: node.type_id.clone(),
                    param: name.clone(),
                })?;
            ps.validate(&param.value)
                .map_err(GraphError::InvalidValue)?;
        }
        Ok(())
    }

    fn validate_wire(&self, registry: &Registry, wire: &Wire) -> Result<(), GraphError> {
        let from_schema = self.schema(registry, wire.from.node)?;
        let to_schema = self.schema(registry, wire.to.node)?;
        let out = Self::socket(from_schema, true, &wire.from.socket)?;
        let inp = Self::socket(to_schema, false, &wire.to.socket)?;
        if out.wire != inp.wire {
            return Err(GraphError::WireTypeMismatch {
                from: out.wire,
                to: inp.wire,
            });
        }
        let describe = || {
            format!(
                "{}.{} → {}.{}",
                wire.from.node, wire.from.socket, wire.to.node, wire.to.socket
            )
        };
        if !inp.feedback
            && (wire.from.node == wire.to.node
                || self.reaches(registry, wire.to.node, wire.from.node))
        {
            return Err(GraphError::Cycle(describe()));
        }
        Ok(())
    }

    /// Validates and applies one edit, returning its inverse.
    pub fn apply(&mut self, registry: &Registry, edit: Edit) -> Result<Applied, GraphError> {
        match edit {
            Edit::AddNode { node } => {
                if self.nodes.contains_key(&node.id)
                    || self.comments.contains_key(&CommentId(node.id.0))
                {
                    return Err(GraphError::DuplicateId(node.id));
                }
                self.validate_node(registry, &node)?;
                let id = node.id;
                self.next_id = self.next_id.max(id.0 + 1);
                self.nodes.insert(id, node);
                Ok(Applied {
                    inverse: vec![Edit::RemoveNode { id }],
                    dirty: vec![id],
                })
            }
            Edit::RemoveNode { id } => {
                if !self.nodes.contains_key(&id) {
                    return Err(GraphError::NoNode(id));
                }
                let wires: Vec<Wire> = self
                    .wires()
                    .filter(|w| w.from.node == id || w.to.node == id)
                    .collect();
                let dirty: Vec<NodeId> = self
                    .downstream_of(id)
                    .into_iter()
                    .filter(|&n| n != id)
                    .collect();
                for w in &wires {
                    self.inputs.remove(&w.to);
                }
                let node = self.nodes.remove(&id).expect("checked above");
                Ok(Applied {
                    inverse: vec![Edit::RestoreNode { node, wires }],
                    dirty,
                })
            }
            Edit::RestoreNode { node, wires } => {
                if self.nodes.contains_key(&node.id) {
                    return Err(GraphError::DuplicateId(node.id));
                }
                self.validate_node(registry, &node)?;
                let id = node.id;
                self.next_id = self.next_id.max(id.0 + 1);
                self.nodes.insert(id, node);
                for w in &wires {
                    if let Err(e) = self.validate_wire(registry, w).and_then(|()| {
                        if self.inputs.contains_key(&w.to) {
                            Err(GraphError::Cycle(format!(
                                "input {}.{} is occupied",
                                w.to.node, w.to.socket
                            )))
                        } else {
                            Ok(())
                        }
                    }) {
                        // Undo the partial restore before reporting.
                        for done in wires.iter().take_while(|d| *d != w) {
                            self.inputs.remove(&done.to);
                        }
                        self.nodes.remove(&id);
                        return Err(e);
                    }
                    self.inputs.insert(w.to.clone(), w.from.clone());
                }
                let mut dirty: Vec<NodeId> = vec![id];
                dirty.extend(wires.iter().map(|w| w.to.node).filter(|&n| n != id));
                Ok(Applied {
                    inverse: vec![Edit::RemoveNode { id }],
                    dirty,
                })
            }
            Edit::Connect { wire } => {
                self.validate_wire(registry, &wire)?;
                let old = self.inputs.insert(wire.to.clone(), wire.from.clone());
                let mut inverse = vec![Edit::Disconnect { wire: wire.clone() }];
                if let Some(from) = old {
                    inverse.push(Edit::Connect {
                        wire: Wire::new(from, wire.to.clone()),
                    });
                }
                Ok(Applied {
                    inverse,
                    dirty: vec![wire.to.node],
                })
            }
            Edit::Disconnect { wire } => {
                if self.inputs.get(&wire.to) != Some(&wire.from) {
                    return Err(GraphError::NoWire(format!(
                        "{}.{} → {}.{}",
                        wire.from.node, wire.from.socket, wire.to.node, wire.to.socket
                    )));
                }
                self.inputs.remove(&wire.to);
                let node = wire.to.node;
                Ok(Applied {
                    inverse: vec![Edit::Connect { wire }],
                    dirty: vec![node],
                })
            }
            Edit::SetParam { node, param, value } => {
                let schema = self.schema(registry, node)?;
                let ps = schema
                    .params
                    .iter()
                    .find(|p| p.name == param)
                    .ok_or_else(|| GraphError::NoParam {
                        type_id: schema.type_id.clone(),
                        param: param.clone(),
                    })?;
                if let Some(v) = &value {
                    ps.validate(&v.value).map_err(GraphError::InvalidValue)?;
                    if let Some(bind) = &v.bind {
                        self.validate_binding(registry, node, ps, bind)?;
                    }
                }
                // A constant equal to the default is stored as "no override".
                let value =
                    value.filter(|v| v.expr.is_some() || v.bind.is_some() || v.value != ps.default);
                let n = self
                    .nodes
                    .get_mut(&node)
                    .expect("schema lookup checked the node");
                let old = match value {
                    Some(v) => n.params.insert(param.clone(), v),
                    None => n.params.remove(&param),
                };
                Ok(Applied {
                    inverse: vec![Edit::SetParam {
                        node,
                        param,
                        value: old,
                    }],
                    dirty: vec![node],
                })
            }
            Edit::Rename { node, name } => {
                if !valid_name(&name) {
                    return Err(GraphError::InvalidName(name));
                }
                if self.nodes.values().any(|n| n.name == name && n.id != node) {
                    return Err(GraphError::DuplicateName(name));
                }
                let n = self.nodes.get_mut(&node).ok_or(GraphError::NoNode(node))?;
                let old = std::mem::replace(&mut n.name, name);
                Ok(Applied {
                    inverse: vec![Edit::Rename { node, name: old }],
                    dirty: vec![],
                })
            }
            Edit::Move { node, pos } => {
                let n = self.nodes.get_mut(&node).ok_or(GraphError::NoNode(node))?;
                let old = std::mem::replace(&mut n.pos, pos);
                Ok(Applied {
                    inverse: vec![Edit::Move { node, pos: old }],
                    dirty: vec![],
                })
            }
            Edit::AddComment { comment } => {
                let id = comment.id;
                if self.comments.contains_key(&id) || self.nodes.contains_key(&NodeId(id.0)) {
                    return Err(GraphError::CommentIdInUse(id));
                }
                self.next_id = self.next_id.max(id.0 + 1);
                self.comments.insert(id, comment);
                Ok(Applied {
                    inverse: vec![Edit::RemoveComment { id }],
                    dirty: vec![],
                })
            }
            Edit::RemoveComment { id } => {
                let comment = self.comments.remove(&id).ok_or(GraphError::NoComment(id))?;
                Ok(Applied {
                    inverse: vec![Edit::AddComment { comment }],
                    dirty: vec![],
                })
            }
            Edit::UpdateComment { comment } => {
                let slot = self
                    .comments
                    .get_mut(&comment.id)
                    .ok_or(GraphError::NoComment(comment.id))?;
                let old = std::mem::replace(slot, comment);
                Ok(Applied {
                    inverse: vec![Edit::UpdateComment { comment: old }],
                    dirty: vec![],
                })
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::registry::{OpFamily, ParamKind, ParamSchema};
    use serde_json::json;

    fn socket(name: &str, wire: WireType, feedback: bool) -> SocketSchema {
        SocketSchema {
            name: name.into(),
            wire,
            optional: true,
            feedback,
        }
    }

    fn op(
        type_id: &str,
        family: OpFamily,
        inputs: Vec<SocketSchema>,
        outputs: Vec<SocketSchema>,
    ) -> OperatorSchema {
        OperatorSchema {
            type_id: type_id.into(),
            family,
            label: type_id.into(),
            summary: "test operator".into(),
            version: 1,
            params: vec![ParamSchema {
                name: "gain".into(),
                label: "Gain".into(),
                kind: ParamKind::Float {
                    min: Some(0.0),
                    max: Some(10.0),
                },
                default: json!(1.0),
                help: String::new(),
            }],
            inputs,
            outputs,
            capabilities: vec![],
            cook_budget_us: 100,
            time_dependent: false,
            sink: false,
        }
    }

    /// A small registry of test-only operators.
    pub(crate) fn test_registry() -> Registry {
        let mut r = Registry::default();
        let img = WireType::Img;
        r.register(op(
            "img.gen",
            OpFamily::Img,
            vec![],
            vec![socket("out", img, false)],
        ))
        .unwrap();
        r.register(op(
            "img.mix",
            OpFamily::Img,
            vec![socket("a", img, false), socket("b", img, false)],
            vec![socket("out", img, false)],
        ))
        .unwrap();
        r.register(op(
            "img.feedback",
            OpFamily::Img,
            vec![socket("in", img, false), socket("prev", img, true)],
            vec![socket("out", img, false)],
        ))
        .unwrap();
        let mut lfo = op(
            "sig.lfo",
            OpFamily::Sig,
            vec![],
            vec![socket("out", WireType::Sig, false)],
        );
        lfo.time_dependent = true;
        r.register(lfo).unwrap();
        r
    }

    pub(crate) fn add(net: &mut Network, reg: &Registry, type_id: &str) -> NodeId {
        let id = net.alloc_id();
        let base = type_id.split('.').nth(1).unwrap();
        let name = net.unique_name(base);
        net.apply(
            reg,
            Edit::AddNode {
                node: Node::new(id, type_id, name),
            },
        )
        .unwrap();
        id
    }

    pub(crate) fn connect(
        net: &mut Network,
        reg: &Registry,
        from: NodeId,
        to: NodeId,
        input: &str,
    ) -> Applied {
        net.apply(
            reg,
            Edit::Connect {
                wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
            },
        )
        .unwrap()
    }

    #[test]
    fn ids_are_stable_and_never_reused() {
        let reg = test_registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.gen");
        let b = add(&mut net, &reg, "img.gen");
        assert_eq!((a, b), (NodeId(0), NodeId(1)));
        assert_eq!(net.node(b).unwrap().name, "gen2");
        net.apply(&reg, Edit::RemoveNode { id: b }).unwrap();
        assert_eq!(add(&mut net, &reg, "img.gen"), NodeId(2));
    }

    #[test]
    fn names_are_validated_and_unique() {
        let reg = test_registry();
        let mut net = Network::default();
        let a = add(&mut net, &reg, "img.gen");
        let id = net.alloc_id();
        let dup = Node::new(id, "img.gen", "gen1");
        assert_eq!(
            net.apply(&reg, Edit::AddNode { node: dup }),
            Err(GraphError::DuplicateName("gen1".into()))
        );
        for bad in ["", "1abc", "a b", "a/b"] {
            assert!(matches!(
                net.apply(
                    &reg,
                    Edit::Rename {
                        node: a,
                        name: bad.into()
                    }
                ),
                Err(GraphError::InvalidName(_))
            ));
        }
        let unknown = Node::new(net.alloc_id(), "img.nope", "x");
        assert!(matches!(
            net.apply(&reg, Edit::AddNode { node: unknown }),
            Err(GraphError::UnknownType(_))
        ));
    }

    #[test]
    fn wires_are_typed_and_inputs_single() {
        let reg = test_registry();
        let mut net = Network::default();
        let g1 = add(&mut net, &reg, "img.gen");
        let g2 = add(&mut net, &reg, "img.gen");
        let mix = add(&mut net, &reg, "img.mix");
        let lfo = add(&mut net, &reg, "sig.lfo");

        connect(&mut net, &reg, g1, mix, "a");
        let replaced = connect(&mut net, &reg, g2, mix, "a");
        assert_eq!(net.inputs_of(mix).count(), 1, "an input holds one wire");
        assert_eq!(
            replaced.inverse.len(),
            2,
            "inverse restores the replaced wire"
        );

        let err = net
            .apply(
                &reg,
                Edit::Connect {
                    wire: Wire::new(Endpoint::new(lfo, "out"), Endpoint::new(mix, "b")),
                },
            )
            .unwrap_err();
        assert_eq!(
            err,
            GraphError::WireTypeMismatch {
                from: WireType::Sig,
                to: WireType::Img
            }
        );

        let err = net
            .apply(
                &reg,
                Edit::Connect {
                    wire: Wire::new(Endpoint::new(g1, "out"), Endpoint::new(mix, "zz")),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::NoSocket { kind: "input", .. }));
    }

    #[test]
    fn cycles_need_a_feedback_input() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let fb = add(&mut net, &reg, "img.feedback");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, g, fb, "in");
        connect(&mut net, &reg, fb, mix, "a");
        // mix → fb.in would loop.
        let err = net
            .apply(
                &reg,
                Edit::Connect {
                    wire: Wire::new(Endpoint::new(mix, "out"), Endpoint::new(fb, "in")),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::Cycle(_)));
        // mix → fb.prev is a feedback loop and allowed.
        connect(&mut net, &reg, mix, fb, "prev");
        // A self-loop through a normal input is rejected.
        let err = net
            .apply(
                &reg,
                Edit::Connect {
                    wire: Wire::new(Endpoint::new(mix, "out"), Endpoint::new(mix, "b")),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::Cycle(_)));
    }

    #[test]
    fn params_store_only_overrides() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        assert_eq!(net.param_value(&reg, g, "gain"), Some(&json!(1.0)));
        net.apply(
            &reg,
            Edit::SetParam {
                node: g,
                param: "gain".into(),
                value: Some(Param::constant(json!(2.5))),
            },
        )
        .unwrap();
        assert_eq!(net.param_value(&reg, g, "gain"), Some(&json!(2.5)));
        // Setting the default removes the override.
        net.apply(
            &reg,
            Edit::SetParam {
                node: g,
                param: "gain".into(),
                value: Some(Param::constant(json!(1.0))),
            },
        )
        .unwrap();
        assert!(net.node(g).unwrap().params.is_empty());
        let err = net
            .apply(
                &reg,
                Edit::SetParam {
                    node: g,
                    param: "gain".into(),
                    value: Some(Param::constant(json!(99))),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::InvalidValue(_)));
        let err = net
            .apply(
                &reg,
                Edit::SetParam {
                    node: g,
                    param: "nope".into(),
                    value: Some(Param::constant(json!(1))),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::NoParam { .. }));
    }

    #[test]
    fn every_edit_inverts_exactly() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let fb = add(&mut net, &reg, "img.feedback");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, g, fb, "in");
        connect(&mut net, &reg, fb, mix, "a");
        connect(&mut net, &reg, g, mix, "b");
        connect(&mut net, &reg, mix, fb, "prev");
        let note = net.alloc_comment_id();
        net.apply(
            &reg,
            Edit::AddComment {
                comment: Comment {
                    id: note,
                    text: "feedback loop".into(),
                    pos: [-20.0, -20.0],
                    size: [400.0, 200.0],
                },
            },
        )
        .unwrap();
        // IDs are allocated before use (and never reused, even on undo).
        let fresh = net.alloc_comment_id();

        let edits = vec![
            Edit::RemoveNode { id: fb },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(fb, "out"), Endpoint::new(mix, "b")),
            },
            Edit::Disconnect {
                wire: Wire::new(Endpoint::new(g, "out"), Endpoint::new(mix, "b")),
            },
            Edit::SetParam {
                node: mix,
                param: "gain".into(),
                value: Some(Param::constant(json!(3.0))),
            },
            Edit::Rename {
                node: g,
                name: "source".into(),
            },
            Edit::Move {
                node: g,
                pos: [10.0, -4.0],
            },
            Edit::AddComment {
                comment: Comment {
                    id: fresh,
                    text: "new".into(),
                    pos: [0.0, 0.0],
                    size: [100.0, 50.0],
                },
            },
            Edit::RemoveComment { id: note },
            Edit::UpdateComment {
                comment: Comment {
                    id: note,
                    text: "edited".into(),
                    pos: [5.0, 6.0],
                    size: [300.0, 200.0],
                },
            },
        ];
        for edit in edits {
            let before = net.clone();
            let applied = net
                .apply(&reg, edit.clone())
                .unwrap_or_else(|e| panic!("{edit:?}: {e}"));
            assert_ne!(net, before, "{edit:?} changed nothing");
            for inv in applied.inverse {
                net.apply(&reg, inv).unwrap();
            }
            assert_eq!(net, before, "inverse of {edit:?} restores the network");
        }
    }

    #[test]
    fn remove_node_dirties_downstream_and_restores_wires() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, g, mix, "a");
        let applied = net.apply(&reg, Edit::RemoveNode { id: g }).unwrap();
        assert_eq!(applied.dirty, vec![mix]);
        assert_eq!(net.wires().count(), 0);
        net.apply(&reg, applied.inverse[0].clone()).unwrap();
        assert_eq!(
            net.inputs_of(mix)
                .map(|(i, s)| (i.to_owned(), s.node))
                .collect::<Vec<_>>(),
            [("a".into(), g)]
        );
    }

    #[test]
    fn bindings_validate_and_count_as_dependencies() {
        let reg = test_registry();
        let mut net = Network::default();
        let lfo = add(&mut net, &reg, "sig.lfo");
        let g = add(&mut net, &reg, "img.gen");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, g, mix, "a");
        let bind = |src: NodeId| Param::bound(json!(1.0), src, "");

        net.apply(
            &reg,
            Edit::SetParam {
                node: g,
                param: "gain".into(),
                value: Some(bind(lfo)),
            },
        )
        .unwrap();
        assert_eq!(
            net.bindings_of(g).map(|b| b.node).collect::<Vec<_>>(),
            [lfo]
        );
        assert!(
            net.downstream_of(lfo).contains(&g),
            "bound nodes are downstream"
        );

        // Binding to a node without a SIG output is rejected.
        let err = net
            .apply(
                &reg,
                Edit::SetParam {
                    node: mix,
                    param: "gain".into(),
                    value: Some(bind(g)),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::InvalidBinding(_)), "{err:?}");

        // A binding may not close a loop: lfo's own parameter cannot follow lfo.
        let err = net
            .apply(
                &reg,
                Edit::SetParam {
                    node: lfo,
                    param: "gain".into(),
                    value: Some(bind(lfo)),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::Cycle(_)));

        // A bound parameter is kept even when its fallback equals the default.
        assert!(net.node(g).unwrap().params.contains_key("gain"));
    }

    #[test]
    fn serialisation_is_deterministic() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let mix = add(&mut net, &reg, "img.mix");
        connect(&mut net, &reg, g, mix, "b");
        connect(&mut net, &reg, g, mix, "a");
        let a = serde_json::to_string(&net).unwrap();
        let back: Network = serde_json::from_str(&a).unwrap();
        assert_eq!(back, net);
        assert_eq!(serde_json::to_string(&back).unwrap(), a);
        assert!(
            a.contains("\"wires\":[{\"from\""),
            "wires serialise as a list: {a}"
        );
    }

    #[test]
    fn comments_round_trip_and_old_files_still_load() {
        let reg = test_registry();
        let mut net = Network::default();
        let g = add(&mut net, &reg, "img.gen");
        let c = net.alloc_comment_id();
        assert_ne!(c.0, g.0, "comments and nodes share one ID space");
        let comment = Comment {
            id: c,
            text: "Sources\nlive inputs".into(),
            pos: [-10.0, -10.0],
            size: [200.0, 120.0],
        };
        net.apply(
            &reg,
            Edit::AddComment {
                comment: comment.clone(),
            },
        )
        .unwrap();
        let text = serde_json::to_string(&net).unwrap();
        assert!(text.contains("\"comments\":[{\"id\":"), "{text}");
        let back: Network = serde_json::from_str(&text).unwrap();
        assert_eq!(back, net);
        assert_eq!(back.comment(c), Some(&comment));
        assert_eq!(serde_json::to_string(&back).unwrap(), text);

        // A comment cannot take a node's ID and vice versa.
        let clash = Comment {
            id: CommentId(g.0),
            ..comment
        };
        assert_eq!(
            net.apply(&reg, Edit::AddComment { comment: clash }),
            Err(GraphError::CommentIdInUse(CommentId(g.0)))
        );
        assert!(
            net.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(NodeId(c.0), "img.gen", "x")
                }
            )
            .is_err()
        );

        // Files without comments (written before they existed) load, and
        // empty comment lists are not written.
        let old = r#"{"next_id":1,"nodes":[{"id":0,"type_id":"img.gen","name":"a","pos":[0,0]}],"wires":[]}"#;
        let net: Network = serde_json::from_str(old).unwrap();
        assert_eq!(
            serde_json::to_string(&net).unwrap(),
            old.replace("[0,0]", "[0.0,0.0]")
        );
    }

    #[test]
    fn deserialisation_rejects_broken_structure() {
        let bad = [
            r#"{"next_id":1,"nodes":[{"id":0,"type_id":"img.gen","name":"a","pos":[0,0]},{"id":0,"type_id":"img.gen","name":"b","pos":[0,0]}],"wires":[]}"#,
            r#"{"next_id":0,"nodes":[{"id":0,"type_id":"img.gen","name":"a","pos":[0,0]}],"wires":[]}"#,
            r#"{"next_id":1,"nodes":[],"wires":[{"from":{"node":0,"socket":"out"},"to":{"node":1,"socket":"a"}}]}"#,
            r#"{"next_id":1,"nodes":[{"id":0,"type_id":"img.gen","name":"a","pos":[0,0]}],"wires":[],"comments":[{"id":0,"text":"","pos":[0,0],"size":[1,1]}]}"#,
            r#"{"next_id":1,"nodes":[],"wires":[],"comments":[{"id":5,"text":"","pos":[0,0],"size":[1,1]}]}"#,
        ];
        for json in bad {
            assert!(serde_json::from_str::<Network>(json).is_err(), "{json}");
        }
    }
}
