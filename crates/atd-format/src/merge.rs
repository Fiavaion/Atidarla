//! Three-way merge of projects by stable ID (FR-F3).
//!
//! [`merge`] combines the changes `ours` and `theirs` each made to a
//! common `base`. Every node, parameter, input wire, comment box, network
//! and project section is a [`Slot`]; a slot changed on one side only takes
//! that side's value, a slot both sides changed the same way takes it once,
//! and a slot both changed differently is a [`Conflict`]:
//!
//! | Kind | When |
//! |---|---|
//! | `param` | the same parameter set differently (value, expression or binding) |
//! | `input` | the same input rewired differently (or unwired on one side) |
//! | `name` | a node renamed differently, or two nodes now share a name |
//! | `type` | a node's operator type changed differently |
//! | `edit-delete` | a node or network edited on one side and deleted on the other |
//! | `add-add` | both sides added the same ID with different content |
//! | `dangling` | a wire or binding refers to a node the merge removed |
//! | `comment` | a comment box changed differently |
//! | `project` | a project setting or section (Pack, mappings, timeline, `[ai]`, model) changed differently |
//!
//! Position-only conflicts (both sides moved the same node to different
//! places) are not conflicts: the node keeps **ours**' position and the
//! merge lists it in [`Merge::auto`].
//!
//! Until every conflict has a [`Resolution`], [`Merge::result`] and
//! [`save`] refuse: an unresolved merge never writes silently. Meanwhile
//! the merge holds a placeholder for each conflict (ours, or nothing where
//! ours would leave a dangling wire), so it can be shown.
//!
//! For Git, [`merge_network_text`] merges one network file and, on
//! conflicts, appends a `# atidarla-conflict` block of TOML comments that
//! [`open_conflicted`] reads back (docs/merge.md). Loading a project whose
//! network files still hold such a block fails, so it cannot be opened
//! and saved over by accident.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use atd_core::graph::Network;
use atd_core::pack::{PackDef, PackSet};
use serde_json::{Value as Json, json};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use crate::{
    AiSettings, FormatError, Mappings, ModelPin, Project, TimelineDoc, mappings_from_str,
    mappings_to_string, network, network_from_str, network_to_string, timeline_from_str,
    timeline_to_string,
};

/// The first line of a conflict block in a network file.
pub const CONFLICT_MARKER: &str = "# atidarla-conflict";

/// Why a slot could not be merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConflictKind {
    Param,
    Input,
    Name,
    Type,
    EditDelete,
    AddAdd,
    Dangling,
    Comment,
    Project,
}

impl ConflictKind {
    pub const ALL: [Self; 9] = [
        Self::Param,
        Self::Input,
        Self::Name,
        Self::Type,
        Self::EditDelete,
        Self::AddAdd,
        Self::Dangling,
        Self::Comment,
        Self::Project,
    ];

    /// The name used in conflict blocks and reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Param => "param",
            Self::Input => "input",
            Self::Name => "name",
            Self::Type => "type",
            Self::EditDelete => "edit-delete",
            Self::AddAdd => "add-add",
            Self::Dangling => "dangling",
            Self::Comment => "comment",
            Self::Project => "project",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// One sentence for the conflict list.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Param => "parameter changed differently on both sides",
            Self::Input => "input rewired differently on both sides",
            Self::Name => "renamed differently, or two nodes share the name",
            Self::Type => "operator type changed differently on both sides",
            Self::EditDelete => "edited on one side, deleted on the other",
            Self::AddAdd => "both sides added this ID with different content",
            Self::Dangling => "refers to a node the merge removed",
            Self::Comment => "comment box changed differently on both sides",
            Self::Project => "project setting changed differently on both sides",
        }
    }
}

/// One mergeable piece of a project. Slots order networks before nodes
/// before node fields, the order resolutions are applied in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    Name,
    Description,
    Resolution,
    Ai,
    Mappings,
    Timeline,
    Model(String),
    Pack(String),
    /// A whole network (added or deleted).
    Network(String),
    /// A whole node with its inputs (added or deleted).
    Node {
        net: String,
        node: u64,
    },
    NodeName {
        net: String,
        node: u64,
    },
    NodeType {
        net: String,
        node: u64,
    },
    NodePos {
        net: String,
        node: u64,
    },
    Param {
        net: String,
        node: u64,
        name: String,
    },
    Input {
        net: String,
        node: u64,
        socket: String,
    },
    Comment {
        net: String,
        id: u64,
    },
}

impl Slot {
    /// The network the slot is in.
    pub fn network(&self) -> Option<&str> {
        match self {
            Self::Network(net)
            | Self::Node { net, .. }
            | Self::NodeName { net, .. }
            | Self::NodeType { net, .. }
            | Self::NodePos { net, .. }
            | Self::Param { net, .. }
            | Self::Input { net, .. }
            | Self::Comment { net, .. } => Some(net),
            _ => None,
        }
    }

    /// The node the slot is on.
    pub fn node(&self) -> Option<u64> {
        match self {
            Self::Node { node, .. }
            | Self::NodeName { node, .. }
            | Self::NodeType { node, .. }
            | Self::NodePos { node, .. }
            | Self::Param { node, .. }
            | Self::Input { node, .. } => Some(*node),
            _ => None,
        }
    }

    /// The path within its network (`node/3/param/gain`), as conflict
    /// blocks in network files write it.
    fn relative(&self) -> Option<String> {
        let full = self.to_string();
        let net = self.network()?;
        full.strip_prefix(&format!("network/{net}/"))
            .map(str::to_owned)
    }

    fn parse(path: &str) -> Option<Self> {
        let p: Vec<&str> = path.split('/').collect();
        let s = |i: usize| p.get(i).map(|s| (*s).to_owned());
        let id = |i: usize| p.get(i).and_then(|s| s.parse::<u64>().ok());
        Some(match p.as_slice() {
            ["name"] => Self::Name,
            ["description"] => Self::Description,
            ["resolution"] => Self::Resolution,
            ["ai"] => Self::Ai,
            ["mappings"] => Self::Mappings,
            ["timeline"] => Self::Timeline,
            ["model", _] => Self::Model(s(1)?),
            ["pack", _] => Self::Pack(s(1)?),
            ["network", _] => Self::Network(s(1)?),
            ["network", _, "comment", _] => Self::Comment {
                net: s(1)?,
                id: id(3)?,
            },
            ["network", _, "node", _] => Self::Node {
                net: s(1)?,
                node: id(3)?,
            },
            ["network", _, "node", _, field] => {
                let (net, node) = (s(1)?, id(3)?);
                match *field {
                    "name" => Self::NodeName { net, node },
                    "type" => Self::NodeType { net, node },
                    "pos" => Self::NodePos { net, node },
                    _ => return None,
                }
            }
            ["network", _, "node", _, "param", _] => Self::Param {
                net: s(1)?,
                node: id(3)?,
                name: s(5)?,
            },
            ["network", _, "node", _, "input", _] => Self::Input {
                net: s(1)?,
                node: id(3)?,
                socket: s(5)?,
            },
            _ => return None,
        })
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name => f.write_str("name"),
            Self::Description => f.write_str("description"),
            Self::Resolution => f.write_str("resolution"),
            Self::Ai => f.write_str("ai"),
            Self::Mappings => f.write_str("mappings"),
            Self::Timeline => f.write_str("timeline"),
            Self::Model(id) => write!(f, "model/{id}"),
            Self::Pack(name) => write!(f, "pack/{name}"),
            Self::Network(net) => write!(f, "network/{net}"),
            Self::Node { net, node } => write!(f, "network/{net}/node/{node}"),
            Self::NodeName { net, node } => write!(f, "network/{net}/node/{node}/name"),
            Self::NodeType { net, node } => write!(f, "network/{net}/node/{node}/type"),
            Self::NodePos { net, node } => write!(f, "network/{net}/node/{node}/pos"),
            Self::Param { net, node, name } => {
                write!(f, "network/{net}/node/{node}/param/{name}")
            }
            Self::Input { net, node, socket } => {
                write!(f, "network/{net}/node/{node}/input/{socket}")
            }
            Self::Comment { net, id } => write!(f, "network/{net}/comment/{id}"),
        }
    }
}

/// The content of a slot.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Settings, networks, nodes and their fields, in the JSON form of
    /// `atd_core::graph` (a parameter is `{ "value": .., "expr"?, "bind"? }`).
    Json(Json),
    Pack(Box<PackDef>),
    Mappings(Box<Mappings>),
    Timeline(Box<TimelineDoc>),
    Ai(Box<AiSettings>),
    Model(ModelPin),
}

impl Value {
    pub fn as_json(&self) -> Option<&Json> {
        match self {
            Self::Json(j) => Some(j),
            _ => None,
        }
    }
}

/// `[ai]` as it is written in `project.toml`.
pub fn ai_text(ai: &AiSettings) -> String {
    let mut doc = DocumentMut::new();
    crate::models::write(&mut doc, ai, &BTreeMap::new());
    doc.to_string()
}

/// A slot's value as text, in the form its file writes it; empty when the
/// slot is absent. [`parse_value`] reads it back.
pub fn value_text(slot: &Slot, value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let text = match value {
        Value::Json(j) => match slot {
            Slot::Param { .. } => network::param_to_toml(j).map(|v| v.to_string()),
            Slot::Network(_) => serde_json::from_value::<Network>(j.clone())
                .map_err(|e| e.to_string())
                .and_then(|n| network_to_string(&n).map_err(|e| e.to_string())),
            _ => network::json_to_toml(j).map(|v| v.to_string()),
        }
        .unwrap_or_else(|_| j.to_string()),
        Value::Pack(def) => crate::pack_to_string(def).unwrap_or_else(|e| e.to_string()),
        Value::Mappings(m) => mappings_to_string(m).unwrap_or_else(|e| e.to_string()),
        Value::Timeline(t) => timeline_to_string(t).unwrap_or_else(|e| e.to_string()),
        Value::Ai(ai) => ai_text(ai),
        Value::Model(pin) => format!("{} {}", pin.licence_class.as_str(), pin.sha256),
    };
    text.trim().to_owned()
}

/// Reads a value typed for `slot` in the form [`value_text`] writes; empty
/// text is "absent". Packs and models are chosen, not typed.
pub fn parse_value(slot: &Slot, text: &str) -> Result<Option<Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let toml_value = || -> Result<Item, String> {
        let doc: DocumentMut = format!("v = {text}")
            .parse()
            .map_err(|e: toml_edit::TomlError| e.message().to_owned())?;
        doc.get("v").cloned().ok_or_else(|| "no value".to_owned())
    };
    let json = match slot {
        Slot::Pack(_) | Slot::Model(_) => {
            return Err("choose base, ours or theirs for this one".into());
        }
        Slot::Mappings => {
            return mappings_from_str(text)
                .map(|m| Some(Value::Mappings(Box::new(m))))
                .map_err(|e| e.to_string());
        }
        Slot::Timeline => {
            return timeline_from_str(text)
                .map(|t| Some(Value::Timeline(Box::new(t))))
                .map_err(|e| e.to_string());
        }
        Slot::Ai => {
            let doc = format!("format = {}\nname = \"\"\n{text}\n", crate::FORMAT);
            return crate::project_from_str(&doc)
                .map(|p| Some(Value::Ai(Box::new(p.ai))))
                .map_err(|e| e.to_string());
        }
        Slot::Network(_) => {
            let net = network_from_str(text).map_err(|e| e.to_string())?;
            serde_json::to_value(&net).map_err(|e| e.to_string())?
        }
        Slot::Param { .. } => network::param_from_toml(&toml_value()?)?,
        _ => {
            let item = toml_value()?;
            network::toml_to_json(item.as_value().ok_or("not a value")?)?
        }
    };
    Ok(Some(Value::Json(json)))
}

/// How a conflict is resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Base,
    Ours,
    Theirs,
    /// A value of the user's own (`None`: remove it / leave it unset).
    Value(Option<Value>),
}

/// A slot both sides changed differently.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub kind: ConflictKind,
    pub slot: Slot,
    /// What the slot is, for people: `main / n3 level1: param gain`.
    pub label: String,
    pub base: Option<Value>,
    pub ours: Option<Value>,
    pub theirs: Option<Value>,
    pub resolution: Option<Resolution>,
}

impl Conflict {
    /// The value `resolution` picks.
    pub fn value_of(&self, resolution: &Resolution) -> Option<Value> {
        match resolution {
            Resolution::Base => self.base.clone(),
            Resolution::Ours => self.ours.clone(),
            Resolution::Theirs => self.theirs.clone(),
            Resolution::Value(v) => v.clone(),
        }
    }

    /// The chosen value; `None` while unresolved.
    pub fn chosen(&self) -> Option<Option<Value>> {
        self.resolution.as_ref().map(|r| self.value_of(r))
    }

    pub fn is_resolved(&self) -> bool {
        self.resolution.is_some()
    }
}

/// A difference the merge settled by rule rather than asking.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoResolved {
    pub slot: Slot,
    pub label: String,
    /// What was kept, e.g. "kept ours' position [10.0, 0.0] over theirs' [0.0, 40.0]".
    pub note: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("{0} merge conflict(s) are unresolved")]
    Unresolved(usize),
    #[error("the merged project is not valid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Format(#[from] FormatError),
}

/// A three-way merge: the merged project (with placeholders while
/// conflicts are open), its conflicts and what was settled by rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Merge {
    merged: Parts,
    pub conflicts: Vec<Conflict>,
    pub auto: Vec<AutoResolved>,
}

impl Merge {
    /// No conflicts at all.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// Conflicts without a resolution.
    pub fn unresolved(&self) -> usize {
        self.conflicts.iter().filter(|c| !c.is_resolved()).count()
    }

    /// Sets (or with `None` clears) the resolution of conflict `index`.
    pub fn resolve(&mut self, index: usize, resolution: Option<Resolution>) {
        if let Some(c) = self.conflicts.get_mut(index) {
            c.resolution = resolution;
        }
    }

    /// The merge as it stands, conflicts at their placeholder (ours) unless
    /// resolved. For showing, never for saving.
    pub fn preview(&self) -> Result<Project, MergeError> {
        self.build(false)
    }

    /// The merged project. Fails while a conflict is unresolved, or when
    /// the chosen values do not fit together (a wire to a removed node,
    /// two nodes with one name).
    pub fn result(&self) -> Result<Project, MergeError> {
        self.build(true)
    }

    fn build(&self, strict: bool) -> Result<Project, MergeError> {
        let open = self.unresolved();
        if strict && open > 0 {
            return Err(MergeError::Unresolved(open));
        }
        let mut parts = self.merged.clone();
        let mut order: Vec<&Conflict> = self.conflicts.iter().collect();
        order.sort_by(|a, b| a.slot.cmp(&b.slot));
        for c in order {
            if let Some(v) = c.chosen() {
                parts
                    .set(&c.slot, v)
                    .map_err(|e| MergeError::Invalid(format!("{}: {e}", c.label)))?;
            }
        }
        if strict {
            parts.check().map_err(MergeError::Invalid)?;
        }
        parts.into_project().map_err(MergeError::Invalid)
    }
}

/// Writes the resolved merge to `dir` in canonical form (replacing any
/// conflict blocks). Refuses while a conflict is unresolved.
pub fn save(dir: &Path, merge: &Merge) -> Result<(), MergeError> {
    let project = merge.result()?;
    crate::save(dir, &project)?;
    Ok(())
}

/// Writes an unresolved merge the way the merge driver leaves it: the
/// placeholder project, with a conflict block in each network file that
/// has conflicts, which [`open_conflicted`] reads back and [`crate::load`]
/// refuses. Conflicts outside networks (project settings, Packs, whole
/// networks) have no file to carry them: then nothing is written.
pub fn save_conflicted(dir: &Path, merge: &Merge) -> Result<(), MergeError> {
    if let Some(c) = merge.conflicts.iter().find(|c| c.slot.relative().is_none()) {
        return Err(MergeError::Invalid(format!(
            "{}: {}; this conflict cannot be written to the project files, \
             resolve it in the app (View > Merge)",
            c.label,
            c.kind.describe()
        )));
    }
    let project = merge.preview()?;
    crate::save(dir, &project)?;
    let mut by_network: BTreeMap<&str, Vec<Conflict>> = BTreeMap::new();
    for c in &merge.conflicts {
        if let Some(net) = c.slot.network() {
            by_network.entry(net).or_default().push(c.clone());
        }
    }
    for (net, conflicts) in by_network {
        let path = dir.join("networks").join(format!("{net}.toml"));
        let io = |source| FormatError::Io {
            path: path.clone(),
            source,
        };
        let mut text = std::fs::read_to_string(&path).map_err(io)?;
        text.push_str(&conflict_block(&conflicts));
        std::fs::write(&path, text).map_err(io)?;
    }
    Ok(())
}

/// Merges `ours` and `theirs`, both changed from `base`.
pub fn merge(base: &Project, ours: &Project, theirs: &Project) -> Merge {
    let (b, o, t) = (Parts::from(base), Parts::from(ours), Parts::from(theirs));
    let mut m = Merger::default();
    let mut out = Parts::default();
    for slot in [
        Slot::Name,
        Slot::Description,
        Slot::Resolution,
        Slot::Ai,
        Slot::Mappings,
        Slot::Timeline,
    ] {
        m.slot(&mut out, [&b, &o, &t], slot, ConflictKind::Project);
    }
    for id in union([&b.models, &o.models, &t.models]) {
        m.slot(
            &mut out,
            [&b, &o, &t],
            Slot::Model(id),
            ConflictKind::Project,
        );
    }
    for name in union([&b.packs, &o.packs, &t.packs]) {
        m.slot(
            &mut out,
            [&b, &o, &t],
            Slot::Pack(name),
            ConflictKind::Project,
        );
    }
    for id in union([&b.networks, &o.networks, &t.networks]) {
        let (bn, on, tn) = (
            b.networks.get(&id),
            o.networks.get(&id),
            t.networks.get(&id),
        );
        match (bn, on, tn) {
            (_, Some(on), Some(tn)) => {
                let empty = NetParts::default();
                let net = m.network(&id, [bn.unwrap_or(&empty), on, tn]);
                out.networks.insert(id, net);
            }
            (None, Some(x), None) | (None, None, Some(x)) => {
                out.networks.insert(id, x.clone());
            }
            // Deleted on one side, unchanged on the other.
            (Some(bn), Some(x), None) | (Some(bn), None, Some(x)) if x == bn => {}
            (Some(_), None, None) | (None, None, None) => {}
            _ => {
                if let Some(on) = on {
                    out.networks.insert(id.clone(), on.clone());
                }
                let slot = Slot::Network(id.clone());
                let get = |p: &Parts| p.get(&slot);
                m.conflicts.push(Conflict {
                    kind: ConflictKind::EditDelete,
                    label: format!("network {id}"),
                    base: get(&b),
                    ours: get(&o),
                    theirs: get(&t),
                    slot,
                    resolution: None,
                });
            }
        }
    }
    m.references(&mut out, [&b, &o, &t]);
    m.conflicts.sort_by(|a, c| a.slot.cmp(&c.slot));
    Merge {
        merged: out,
        conflicts: m.conflicts,
        auto: m.auto,
    }
}

/// The keys of three maps, sorted.
fn union<T>(maps: [&BTreeMap<String, T>; 3]) -> BTreeSet<String> {
    maps.iter().flat_map(|m| m.keys().cloned()).collect()
}

/// The three-way rule: the value both agree on, or the one that changed;
/// `None` when both changed differently.
fn three<T: PartialEq + Clone>(b: Option<&T>, o: Option<&T>, t: Option<&T>) -> Option<Option<T>> {
    if o == t || t == b {
        Some(o.cloned())
    } else if o == b {
        Some(t.cloned())
    } else {
        None
    }
}

#[derive(Default)]
struct Merger {
    conflicts: Vec<Conflict>,
    auto: Vec<AutoResolved>,
}

impl Merger {
    /// Merges one slot into `out`; a conflict leaves ours as placeholder.
    fn slot(&mut self, out: &mut Parts, [b, o, t]: [&Parts; 3], slot: Slot, kind: ConflictKind) {
        let (bv, ov, tv) = (b.get(&slot), o.get(&slot), t.get(&slot));
        match three(bv.as_ref(), ov.as_ref(), tv.as_ref()) {
            Some(v) => {
                let _ = out.set(&slot, v);
            }
            None => {
                let _ = out.set(&slot, ov.clone());
                self.conflicts.push(Conflict {
                    kind,
                    label: label(&slot, [o, t, b]),
                    slot,
                    base: bv,
                    ours: ov,
                    theirs: tv,
                    resolution: None,
                });
            }
        }
    }

    fn network(&mut self, id: &str, [b, o, t]: [&NetParts; 3]) -> NetParts {
        let mut out = NetParts {
            next_id: b.next_id.max(o.next_id).max(t.next_id),
            ..Default::default()
        };
        // Single-network views so the slot functions apply.
        let wrap = |n: &NetParts| Parts {
            networks: BTreeMap::from([(id.to_owned(), n.clone())]),
            ..Default::default()
        };
        let (pb, po, pt) = (wrap(b), wrap(o), wrap(t));
        let mut pout = wrap(&out);
        let ids: BTreeSet<u64> = [b, o, t]
            .iter()
            .flat_map(|n| n.nodes.keys().copied())
            .collect();
        for node in ids {
            let (bn, on, tn) = (b.nodes.get(&node), o.nodes.get(&node), t.nodes.get(&node));
            let net = id.to_owned();
            let whole = Slot::Node {
                net: net.clone(),
                node,
            };
            match (bn, on, tn) {
                (Some(bn), Some(on), Some(tn)) => {
                    pout.net_mut(id).nodes.insert(node, on.clone());
                    let mut fields = vec![
                        (
                            Slot::NodeName {
                                net: net.clone(),
                                node,
                            },
                            ConflictKind::Name,
                        ),
                        (
                            Slot::NodeType {
                                net: net.clone(),
                                node,
                            },
                            ConflictKind::Type,
                        ),
                    ];
                    for name in union([&bn.params, &on.params, &tn.params]) {
                        let slot = Slot::Param {
                            net: net.clone(),
                            node,
                            name,
                        };
                        fields.push((slot, ConflictKind::Param));
                    }
                    for socket in union([&bn.inputs, &on.inputs, &tn.inputs]) {
                        let slot = Slot::Input {
                            net: net.clone(),
                            node,
                            socket,
                        };
                        fields.push((slot, ConflictKind::Input));
                    }
                    for (slot, kind) in fields {
                        self.slot(&mut pout, [&pb, &po, &pt], slot, kind);
                    }
                    if three(Some(&bn.pos), Some(&on.pos), Some(&tn.pos)).is_none() {
                        let slot = Slot::NodePos { net, node };
                        self.auto.push(AutoResolved {
                            label: label(&slot, [&po, &pt, &pb]),
                            note: format!(
                                "moved on both sides: kept ours' position {} over theirs' {}",
                                on.pos, tn.pos
                            ),
                            slot,
                        });
                    }
                }
                (None, Some(x), None) | (None, None, Some(x)) => {
                    pout.net_mut(id).nodes.insert(node, x.clone());
                }
                (None, Some(on), Some(tn)) if on == tn => {
                    pout.net_mut(id).nodes.insert(node, on.clone());
                }
                (Some(bn), Some(x), None) | (Some(bn), None, Some(x)) if x == bn => {}
                (Some(_), None, None) | (None, None, None) => {}
                _ => {
                    let kind = if bn.is_none() {
                        ConflictKind::AddAdd
                    } else {
                        ConflictKind::EditDelete
                    };
                    if let Some(on) = on {
                        pout.net_mut(id).nodes.insert(node, on.clone());
                    }
                    self.conflicts.push(Conflict {
                        kind,
                        label: label(&whole, [&po, &pt, &pb]),
                        base: pb.get(&whole),
                        ours: po.get(&whole),
                        theirs: pt.get(&whole),
                        slot: whole,
                        resolution: None,
                    });
                }
            }
        }
        let comments: BTreeSet<u64> = [b, o, t]
            .iter()
            .flat_map(|n| n.comments.keys().copied())
            .collect();
        for cid in comments {
            let slot = Slot::Comment {
                net: id.to_owned(),
                id: cid,
            };
            self.slot(&mut pout, [&pb, &po, &pt], slot, ConflictKind::Comment);
        }
        out.nodes = std::mem::take(&mut pout.net_mut(id).nodes);
        out.comments = std::mem::take(&mut pout.net_mut(id).comments);
        out
    }

    /// Wires and bindings to nodes the merge removed, comments whose ID a
    /// node took, and duplicate names become conflicts; their placeholder
    /// drops the reference so the merge stays a valid network.
    fn references(&mut self, out: &mut Parts, sides: [&Parts; 3]) {
        let mut found = Vec::new();
        for (net_id, net) in &out.networks {
            let ids: BTreeSet<u64> = net.nodes.keys().copied().collect();
            let mut names = BTreeSet::new();
            for (&node, parts) in &net.nodes {
                let net = net_id.clone();
                for (socket, from) in &parts.inputs {
                    if !from["node"].as_u64().is_some_and(|n| ids.contains(&n)) {
                        let socket = socket.clone();
                        let slot = Slot::Input {
                            net: net.clone(),
                            node,
                            socket,
                        };
                        found.push((slot, ConflictKind::Dangling, true));
                    }
                }
                for (name, p) in &parts.params {
                    if p["bind"]["node"]
                        .as_u64()
                        .is_some_and(|n| !ids.contains(&n))
                    {
                        let name = name.clone();
                        let slot = Slot::Param {
                            net: net.clone(),
                            node,
                            name,
                        };
                        found.push((slot, ConflictKind::Dangling, true));
                    }
                }
                if !names.insert(parts.name.to_string()) {
                    found.push((Slot::NodeName { net, node }, ConflictKind::Name, false));
                }
            }
            for &id in net.comments.keys() {
                if ids.contains(&id) {
                    let slot = Slot::Comment {
                        net: net_id.clone(),
                        id,
                    };
                    found.push((slot, ConflictKind::AddAdd, true));
                }
            }
        }
        for (slot, kind, drop) in found {
            if drop {
                let _ = out.set(&slot, None);
            }
            if self.conflicts.iter().any(|c| c.slot == slot) {
                continue;
            }
            let [b, o, t] = sides;
            self.conflicts.push(Conflict {
                kind,
                label: label(&slot, [o, t, b]),
                base: b.get(&slot),
                ours: o.get(&slot),
                theirs: t.get(&slot),
                slot,
                resolution: None,
            });
        }
    }
}

/// `main / n3 level1: param gain`, naming the node from the first side
/// that has it.
fn label(slot: &Slot, sides: [&Parts; 3]) -> String {
    let Some(net) = slot.network() else {
        return slot.to_string();
    };
    let what = match slot {
        Slot::Network(_) => return format!("network {net}"),
        Slot::Comment { id, .. } => return format!("{net} / comment c{id}"),
        Slot::Node { .. } => "node".to_owned(),
        Slot::NodeName { .. } => "name".to_owned(),
        Slot::NodeType { .. } => "type".to_owned(),
        Slot::NodePos { .. } => "position".to_owned(),
        Slot::Param { name, .. } => format!("param {name}"),
        Slot::Input { socket, .. } => format!("input {socket}"),
        _ => String::new(),
    };
    let node = slot.node().unwrap_or(0);
    let name = sides
        .iter()
        .find_map(|p| p.networks.get(net)?.nodes.get(&node)?.name.as_str())
        .unwrap_or("?");
    format!("{net} / n{node} {name}: {what}")
}

/// A project split into slots.
#[derive(Debug, Clone, Default, PartialEq)]
struct Parts {
    name: String,
    description: String,
    resolution: [u32; 2],
    ai: AiSettings,
    mappings: Mappings,
    timeline: Option<TimelineDoc>,
    models: BTreeMap<String, ModelPin>,
    packs: BTreeMap<String, PackDef>,
    networks: BTreeMap<String, NetParts>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct NetParts {
    next_id: u64,
    nodes: BTreeMap<u64, NodeParts>,
    comments: BTreeMap<u64, Json>,
}

#[derive(Debug, Clone, PartialEq)]
struct NodeParts {
    name: Json,
    type_id: Json,
    pos: Json,
    params: BTreeMap<String, Json>,
    /// Input socket to its source `{ node, socket }`.
    inputs: BTreeMap<String, Json>,
}

impl NodeParts {
    fn to_json(&self) -> Json {
        json!({
            "name": self.name,
            "type": self.type_id,
            "pos": self.pos,
            "params": self.params,
            "inputs": self.inputs,
        })
    }

    fn from_json(j: &Json) -> Result<Self, String> {
        let map = |key: &str| -> Result<BTreeMap<String, Json>, String> {
            match &j[key] {
                Json::Null => Ok(BTreeMap::new()),
                Json::Object(m) => Ok(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                _ => Err(format!("`{key}` must be a table")),
            }
        };
        if !j["name"].is_string() || !j["type"].is_string() {
            return Err("a node needs `name` and `type`".into());
        }
        Ok(Self {
            name: j["name"].clone(),
            type_id: j["type"].clone(),
            pos: if j["pos"].is_null() {
                json!([0.0, 0.0])
            } else {
                j["pos"].clone()
            },
            params: map("params")?,
            inputs: map("inputs")?,
        })
    }
}

impl NetParts {
    fn from_network(net: &Network) -> Self {
        let repr = serde_json::to_value(net).unwrap_or_default();
        let mut out = Self {
            next_id: repr["next_id"].as_u64().unwrap_or(0),
            ..Default::default()
        };
        for n in repr["nodes"].as_array().into_iter().flatten() {
            let params = n["params"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            out.nodes.insert(
                n["id"].as_u64().unwrap_or(0),
                NodeParts {
                    name: n["name"].clone(),
                    type_id: n["type_id"].clone(),
                    pos: n["pos"].clone(),
                    params,
                    inputs: BTreeMap::new(),
                },
            );
        }
        for w in repr["wires"].as_array().into_iter().flatten() {
            let to = &w["to"];
            if let (Some(node), Some(socket)) = (to["node"].as_u64(), to["socket"].as_str())
                && let Some(n) = out.nodes.get_mut(&node)
            {
                n.inputs.insert(socket.to_owned(), w["from"].clone());
            }
        }
        for c in repr["comments"].as_array().into_iter().flatten() {
            out.comments
                .insert(c["id"].as_u64().unwrap_or(0), c.clone());
        }
        out
    }

    fn to_json(&self) -> Json {
        let mut nodes = Vec::new();
        let mut wires = Vec::new();
        for (id, n) in &self.nodes {
            nodes.push(json!({
                "id": id,
                "type_id": n.type_id,
                "name": n.name,
                "pos": n.pos,
                "params": n.params,
            }));
            for (socket, from) in &n.inputs {
                wires.push(json!({ "from": from, "to": { "node": id, "socket": socket } }));
            }
        }
        json!({
            "next_id": self.next_id,
            "nodes": nodes,
            "wires": wires,
            "comments": self.comments.values().collect::<Vec<_>>(),
        })
    }

    fn to_network(&self) -> Result<Network, String> {
        serde_json::from_value(self.to_json()).map_err(|e| e.to_string())
    }
}

impl From<&Project> for Parts {
    fn from(p: &Project) -> Self {
        Self {
            name: p.name.clone(),
            description: p.description.clone(),
            resolution: p.resolution,
            ai: p.ai.clone(),
            mappings: p.mappings.clone(),
            timeline: p.timeline.clone(),
            models: p.models.clone(),
            packs: p
                .packs
                .iter()
                .map(|d| (d.name.clone(), d.clone()))
                .collect(),
            networks: p
                .networks
                .iter()
                .map(|(id, n)| (id.clone(), NetParts::from_network(n)))
                .collect(),
        }
    }
}

impl Parts {
    fn net_mut(&mut self, id: &str) -> &mut NetParts {
        self.networks.entry(id.to_owned()).or_default()
    }

    fn node(&self, net: &str, node: u64) -> Option<&NodeParts> {
        self.networks.get(net)?.nodes.get(&node)
    }

    fn get(&self, slot: &Slot) -> Option<Value> {
        let j = |v: &Json| Some(Value::Json(v.clone()));
        match slot {
            Slot::Name => j(&json!(self.name)),
            Slot::Description => j(&json!(self.description)),
            Slot::Resolution => j(&json!(self.resolution)),
            Slot::Ai => Some(Value::Ai(Box::new(self.ai.clone()))),
            Slot::Mappings => Some(Value::Mappings(Box::new(self.mappings.clone()))),
            Slot::Timeline => self
                .timeline
                .as_ref()
                .map(|t| Value::Timeline(Box::new(t.clone()))),
            Slot::Model(id) => self.models.get(id).map(|m| Value::Model(m.clone())),
            Slot::Pack(name) => self
                .packs
                .get(name)
                .map(|d| Value::Pack(Box::new(d.clone()))),
            // As `atd_core` writes a network, so it compares with its text.
            Slot::Network(id) => self
                .networks
                .get(id)
                .and_then(|n| serde_json::to_value(n.to_network().ok()?).ok())
                .map(Value::Json),
            Slot::Node { net, node } => self.node(net, *node).map(|n| Value::Json(n.to_json())),
            Slot::NodeName { net, node } => j(&self.node(net, *node)?.name),
            Slot::NodeType { net, node } => j(&self.node(net, *node)?.type_id),
            Slot::NodePos { net, node } => j(&self.node(net, *node)?.pos),
            Slot::Param { net, node, name } => j(self.node(net, *node)?.params.get(name)?),
            Slot::Input { net, node, socket } => j(self.node(net, *node)?.inputs.get(socket)?),
            Slot::Comment { net, id } => j(self.networks.get(net)?.comments.get(id)?),
        }
    }

    /// Sets a slot. Setting a field of a node that is not there does
    /// nothing (a resolution removed the node).
    fn set(&mut self, slot: &Slot, value: Option<Value>) -> Result<(), String> {
        let wrong = || format!("{slot}: wrong kind of value");
        let json = |v: &Option<Value>| -> Result<Option<Json>, String> {
            match v {
                None => Ok(None),
                Some(Value::Json(j)) => Ok(Some(j.clone())),
                Some(_) => Err(wrong()),
            }
        };
        let string = |v: &Option<Value>| -> Result<String, String> {
            Ok(json(v)?
                .map(|j| j.as_str().map(str::to_owned).ok_or_else(wrong))
                .transpose()?
                .unwrap_or_default())
        };
        match slot {
            Slot::Name => self.name = string(&value)?,
            Slot::Description => self.description = string(&value)?,
            Slot::Resolution => {
                let j = json(&value)?.ok_or_else(wrong)?;
                self.resolution = serde_json::from_value(j).map_err(|e| e.to_string())?;
            }
            Slot::Ai => match value {
                Some(Value::Ai(ai)) => self.ai = *ai,
                None => self.ai = AiSettings::default(),
                _ => return Err(wrong()),
            },
            Slot::Mappings => match value {
                Some(Value::Mappings(m)) => self.mappings = *m,
                None => self.mappings = Mappings::default(),
                _ => return Err(wrong()),
            },
            Slot::Timeline => match value {
                Some(Value::Timeline(t)) => self.timeline = Some(*t),
                None => self.timeline = None,
                _ => return Err(wrong()),
            },
            Slot::Model(id) => match value {
                Some(Value::Model(m)) => {
                    self.models.insert(id.clone(), m);
                }
                None => {
                    self.models.remove(id);
                }
                _ => return Err(wrong()),
            },
            Slot::Pack(name) => match value {
                Some(Value::Pack(d)) => {
                    self.packs.insert(name.clone(), *d);
                }
                None => {
                    self.packs.remove(name);
                }
                _ => return Err(wrong()),
            },
            Slot::Network(id) => match json(&value)? {
                Some(j) => {
                    let net: Network = serde_json::from_value(j).map_err(|e| e.to_string())?;
                    self.networks
                        .insert(id.clone(), NetParts::from_network(&net));
                }
                None => {
                    self.networks.remove(id);
                }
            },
            Slot::Node { net, node } => match json(&value)? {
                Some(j) => {
                    let parts = NodeParts::from_json(&j)?;
                    self.net_mut(net).nodes.insert(*node, parts);
                }
                None => {
                    if let Some(n) = self.networks.get_mut(net) {
                        n.nodes.remove(node);
                    }
                }
            },
            Slot::Comment { net, id } => match json(&value)? {
                Some(mut j) => {
                    j["id"] = json!(id);
                    self.net_mut(net).comments.insert(*id, j);
                }
                None => {
                    if let Some(n) = self.networks.get_mut(net) {
                        n.comments.remove(id);
                    }
                }
            },
            Slot::NodeName { net, node }
            | Slot::NodeType { net, node }
            | Slot::NodePos { net, node }
            | Slot::Param { net, node, .. }
            | Slot::Input { net, node, .. } => {
                let v = json(&value)?;
                let Some(n) = self
                    .networks
                    .get_mut(net)
                    .and_then(|n| n.nodes.get_mut(node))
                else {
                    return Ok(());
                };
                match slot {
                    Slot::NodeName { .. } => n.name = v.ok_or_else(wrong)?,
                    Slot::NodeType { .. } => n.type_id = v.ok_or_else(wrong)?,
                    Slot::NodePos { .. } => n.pos = v.ok_or_else(wrong)?,
                    Slot::Param { name, .. } => match v {
                        Some(p) => {
                            n.params.insert(name.clone(), p);
                        }
                        None => {
                            n.params.remove(name);
                        }
                    },
                    Slot::Input { socket, .. } => match v {
                        Some(e) => {
                            n.inputs.insert(socket.clone(), e);
                        }
                        None => {
                            n.inputs.remove(socket);
                        }
                    },
                    _ => unreachable!("matched above"),
                }
            }
        }
        Ok(())
    }

    /// What a network file cannot say but a merge can produce: bindings
    /// to missing nodes and duplicate names. (Wires to missing nodes fail
    /// when the network is built.)
    fn check(&self) -> Result<(), String> {
        for (id, net) in &self.networks {
            let mut names = BTreeSet::new();
            for (node, n) in &net.nodes {
                if !names.insert(n.name.to_string()) {
                    return Err(format!("network {id}: two nodes are named {}", n.name));
                }
                for (param, p) in &n.params {
                    if let Some(to) = p["bind"]["node"].as_u64()
                        && !net.nodes.contains_key(&to)
                    {
                        return Err(format!(
                            "network {id}: n{node} param {param} is bound to missing node n{to}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn into_project(self) -> Result<Project, String> {
        let mut packs = PackSet::default();
        for def in self.packs.into_values() {
            packs.insert(def);
        }
        let mut networks = BTreeMap::new();
        for (id, n) in &self.networks {
            let net = n.to_network().map_err(|e| format!("network {id}: {e}"))?;
            networks.insert(id.clone(), net);
        }
        Ok(Project {
            name: self.name,
            description: self.description,
            resolution: self.resolution,
            networks,
            packs,
            mappings: self.mappings,
            timeline: self.timeline,
            ai: self.ai,
            models: self.models,
        })
    }
}

/// One network file merged by [`merge_network_text`].
#[derive(Debug, Clone, PartialEq)]
pub struct FileMerge {
    /// The merged file: canonical, followed by a conflict block when
    /// `conflicts` is not zero.
    pub text: String,
    pub conflicts: usize,
    /// Position-only differences kept as ours.
    pub auto: Vec<AutoResolved>,
}

/// Merges three versions of one network file (Git merge driver). An empty
/// `base` means the file is new on both sides.
pub fn merge_network_text(base: &str, ours: &str, theirs: &str) -> Result<FileMerge, FormatError> {
    let read = |text: &str, which: &str| -> Result<Project, FormatError> {
        let net = if text.trim().is_empty() {
            Network::default()
        } else {
            network_from_str(text).map_err(|e| e.in_file(which))?
        };
        Ok(Project::with_main("", net))
    };
    let m = merge(
        &read(base, "base")?,
        &read(ours, "ours")?,
        &read(theirs, "theirs")?,
    );
    let net = m
        .merged
        .networks
        .get(crate::MAIN)
        .map(NetParts::to_network)
        .transpose()
        .map_err(FormatError::Invalid)?
        .unwrap_or_default();
    let mut text = network_to_string(&net)?;
    if !m.conflicts.is_empty() {
        text.push_str(&conflict_block(&m.conflicts));
    }
    Ok(FileMerge {
        text,
        conflicts: m.conflicts.len(),
        auto: m.auto,
    })
}

/// The `# atidarla-conflict` block: a commented-out TOML document of
/// `[[conflict]]` tables with JSON values, so the file stays a valid
/// network (the placeholder merge) and the app can read the choices back.
fn conflict_block(conflicts: &[Conflict]) -> String {
    let mut list = ArrayOfTables::new();
    for c in conflicts {
        let mut t = Table::new();
        t.insert("kind", Item::Value(c.kind.name().into()));
        t.insert(
            "slot",
            Item::Value(
                c.slot
                    .relative()
                    .unwrap_or_else(|| c.slot.to_string())
                    .into(),
            ),
        );
        t.insert("label", Item::Value(c.label.as_str().into()));
        for (key, v) in [("base", &c.base), ("ours", &c.ours), ("theirs", &c.theirs)] {
            if let Some(j) = v.as_ref().and_then(Value::as_json) {
                t.insert(key, Item::Value(j.to_string().into()));
            }
        }
        list.push(t);
    }
    let mut doc = DocumentMut::new();
    doc.insert("conflict", Item::ArrayOfTables(list));
    let mut out = format!(
        "\n{CONFLICT_MARKER}: {} unresolved; the network above is a placeholder (ours). \
         Resolve in Atidarla: View > Merge > Open conflicted (docs/merge.md).\n",
        conflicts.len()
    );
    for line in doc.to_string().lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Whether a network file holds a conflict block.
pub fn has_conflict_block(text: &str) -> bool {
    text.lines().any(|l| l.starts_with(CONFLICT_MARKER))
}

/// The conflicts in a network file's conflict block (`net` is the
/// network's ID); empty when it has none.
pub fn read_conflict_block(text: &str, net: &str) -> Result<Vec<Conflict>, FormatError> {
    let Some(start) = text.lines().position(|l| l.starts_with(CONFLICT_MARKER)) else {
        return Ok(Vec::new());
    };
    let body: String = text
        .lines()
        .skip(start + 1)
        .filter_map(|l| l.strip_prefix('#'))
        .map(|l| format!("{}\n", l.strip_prefix(' ').unwrap_or(l)))
        .collect();
    let bad = |m: String| FormatError::Invalid(format!("conflict block: {m}"));
    let doc: DocumentMut = body
        .parse()
        .map_err(|e: toml_edit::TomlError| bad(e.message().to_owned()))?;
    let mut out = Vec::new();
    for (i, t) in network::tables(doc.get("conflict"), "conflict")?
        .into_iter()
        .enumerate()
    {
        let at = format!("conflict #{}", i + 1);
        let kind_name = network::required_str(t, "kind", &at)?;
        let kind = ConflictKind::from_name(kind_name)
            .ok_or_else(|| bad(format!("{at}: unknown kind `{kind_name}`")))?;
        let path = network::required_str(t, "slot", &at)?;
        let slot = Slot::parse(&format!("network/{net}/{path}"))
            .ok_or_else(|| bad(format!("{at}: unknown slot `{path}`")))?;
        let value = |key: &str| -> Result<Option<Value>, FormatError> {
            t.get(key)
                .map(|_| {
                    let s = network::required_str(t, key, &at)?;
                    serde_json::from_str::<Json>(s)
                        .map(Value::Json)
                        .map_err(|e| bad(format!("{at}: `{key}`: {e}")))
                })
                .transpose()
        };
        out.push(Conflict {
            kind,
            label: t
                .get("label")
                .and_then(Item::as_str)
                .map_or_else(|| slot.to_string(), str::to_owned),
            base: value("base")?,
            ours: value("ours")?,
            theirs: value("theirs")?,
            slot,
            resolution: None,
        });
    }
    Ok(out)
}

/// Opens a project whose network files hold conflict blocks (left by the
/// Git merge driver) as a [`Merge`] to resolve; `None` when it has none.
pub fn open_conflicted(dir: &Path) -> Result<Option<Merge>, MergeError> {
    let project = crate::load_with(dir, true)?;
    let mut conflicts = Vec::new();
    for id in project.networks.keys() {
        let path = dir.join("networks").join(format!("{id}.toml"));
        let text = std::fs::read_to_string(&path).map_err(|source| FormatError::Io {
            path: path.clone(),
            source,
        })?;
        conflicts.extend(
            read_conflict_block(&text, id)
                .map_err(|e| e.in_file(&format!("networks/{id}.toml")))?,
        );
    }
    if conflicts.is_empty() {
        return Ok(None);
    }
    conflicts.sort_by(|a, b| a.slot.cmp(&b.slot));
    Ok(Some(Merge {
        merged: Parts::from(&project),
        conflicts,
        auto: Vec::new(),
    }))
}
