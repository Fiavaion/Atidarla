//! What changed between two states of a project (FR-F2).
//!
//! [`diff`] compares two [`Project`]s by stable ID: per network, the nodes
//! added, removed and changed (renamed, moved, retyped, parameters and
//! input wires), the comment boxes, and the project-level sections at a
//! coarser grain (settings, Packs, mappings, timeline, `[ai]`, models).
//! Everything is listed in ID or name order, so the same two projects
//! always give the same report. A wire belongs to the input it feeds
//! (each input holds at most one), as in the network files.
//!
//! The [`std::fmt::Display`] form is the text report of `atidarla diff`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use atd_core::graph::{Comment, CommentId, Edit, Endpoint, Network, Node, NodeId, Param, Wire};

use crate::{Project, mappings_to_string, network, pack_to_string, timeline_to_string};

/// Whether something exists only in the new state, only in the old one,
/// or in both with changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Presence {
    Added,
    Removed,
    Changed,
}

impl Presence {
    /// The report's marker: `+`, `-` or `~`.
    pub fn sign(self) -> char {
        match self {
            Self::Added => '+',
            Self::Removed => '-',
            Self::Changed => '~',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// The difference between two projects. Empty when they are equal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectDiff {
    /// `name`, `description` and `resolution`.
    pub settings: Vec<SettingChange>,
    /// Networks with changes, by ID.
    pub networks: Vec<NetworkDiff>,
    /// Packs, mappings, timeline, `[ai]` and models.
    pub sections: Vec<SectionChange>,
}

impl ProjectDiff {
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.networks.is_empty() && self.sections.is_empty()
    }

    pub fn network(&self, id: &str) -> Option<&NetworkDiff> {
        self.networks.iter().find(|n| n.id == id)
    }
}

/// A project setting, old and new value as text.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingChange {
    pub key: &'static str,
    pub old: String,
    pub new: String,
}

/// The changes to one network.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkDiff {
    pub id: String,
    pub presence: Presence,
    /// Nodes with changes, by ID.
    pub nodes: Vec<NodeDiff>,
    /// Comment boxes with changes, by ID.
    pub comments: Vec<CommentDiff>,
}

impl NetworkDiff {
    pub fn node(&self, id: NodeId) -> Option<&NodeDiff> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// The changes to one node. An added node lists its parameters and inputs
/// as changes from nothing, a removed one as changes to nothing, so both
/// can be shown in the same table.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDiff {
    pub id: NodeId,
    /// The node's name in the new state (the old state when removed).
    pub name: String,
    /// Its operator type in the new state (the old state when removed).
    pub type_id: String,
    pub presence: Presence,
    pub changes: Vec<NodeChange>,
}

impl NodeDiff {
    /// A changed node whose only change is its canvas position.
    pub fn is_move_only(&self) -> bool {
        self.presence == Presence::Changed
            && self
                .changes
                .iter()
                .all(|c| matches!(c, NodeChange::Moved { .. }))
    }
}

/// One change to a node.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeChange {
    Renamed {
        old: String,
        new: String,
    },
    Retyped {
        old: String,
        new: String,
    },
    Moved {
        old: [f32; 2],
        new: [f32; 2],
    },
    /// A parameter override set, changed (value, expression or binding)
    /// or reset to its default (`None`).
    Param {
        name: String,
        old: Option<Param>,
        new: Option<Param>,
    },
    /// The wire into an input: connected, rewired or disconnected.
    Input {
        socket: String,
        old: Option<Endpoint>,
        new: Option<Endpoint>,
    },
}

/// A comment box added, removed or changed.
#[derive(Debug, Clone, PartialEq)]
pub struct CommentDiff {
    pub id: CommentId,
    pub presence: Presence,
    pub old: Option<Comment>,
    pub new: Option<Comment>,
}

/// A project section compared as a whole.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Pack(String),
    Mappings,
    Timeline,
    Ai,
    Model(String),
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pack(name) => write!(f, "pack {name}"),
            Self::Mappings => f.write_str("mappings"),
            Self::Timeline => f.write_str("timeline"),
            Self::Ai => f.write_str("[ai]"),
            Self::Model(id) => write!(f, "model {id}"),
        }
    }
}

/// A section added, removed or changed, with a one-line summary.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionChange {
    pub section: Section,
    pub presence: Presence,
    pub summary: String,
}

/// What changed from `old` to `new`.
pub fn diff(old: &Project, new: &Project) -> ProjectDiff {
    let mut d = ProjectDiff::default();
    let mut setting = |key, old: String, new: String| {
        if old != new {
            d.settings.push(SettingChange { key, old, new });
        }
    };
    setting("name", quoted(&old.name), quoted(&new.name));
    setting(
        "description",
        quoted(&old.description),
        quoted(&new.description),
    );
    setting(
        "resolution",
        format!("{}x{}", old.resolution[0], old.resolution[1]),
        format!("{}x{}", new.resolution[0], new.resolution[1]),
    );

    let ids: BTreeSet<&String> = old.networks.keys().chain(new.networks.keys()).collect();
    let empty = Network::default();
    for id in ids {
        let (a, b) = (old.networks.get(id), new.networks.get(id));
        let presence = match (a, b) {
            (None, _) => Presence::Added,
            (_, None) => Presence::Removed,
            _ => Presence::Changed,
        };
        let (nodes, comments) = diff_networks(a.unwrap_or(&empty), b.unwrap_or(&empty));
        if presence != Presence::Changed || !nodes.is_empty() || !comments.is_empty() {
            d.networks.push(NetworkDiff {
                id: id.clone(),
                presence,
                nodes,
                comments,
            });
        }
    }

    let packs = |p: &Project| -> BTreeMap<String, atd_core::pack::PackDef> {
        p.packs
            .iter()
            .map(|def| (def.name.clone(), def.clone()))
            .collect()
    };
    keyed(
        &mut d.sections,
        &packs(old),
        &packs(new),
        Section::Pack,
        |a, b| {
            if a.version != b.version {
                format!("version {} → {}", a.version, b.version)
            } else {
                "changed".into()
            }
        },
    );
    let (ma, mb) = (&old.mappings, &new.mappings);
    if ma != mb {
        let presence = match (ma.is_empty(), mb.is_empty()) {
            (true, false) => Presence::Added,
            (false, true) => Presence::Removed,
            _ => Presence::Changed,
        };
        d.sections.push(SectionChange {
            section: Section::Mappings,
            presence,
            summary: format!(
                "{} → {} mappings{}",
                ma.mappings.len(),
                mb.mappings.len(),
                if ma.osc != mb.osc {
                    ", OSC settings changed"
                } else {
                    ""
                }
            ),
        });
    }
    if old.timeline != new.timeline {
        let count = |t: &Option<crate::TimelineDoc>| {
            t.as_ref().map_or((0, 0, 0), |t| {
                (t.tracks.len(), t.markers.len(), t.cues.len())
            })
        };
        let (a, b) = (count(&old.timeline), count(&new.timeline));
        let presence = match (&old.timeline, &new.timeline) {
            (None, _) => Presence::Added,
            (_, None) => Presence::Removed,
            _ => Presence::Changed,
        };
        d.sections.push(SectionChange {
            section: Section::Timeline,
            presence,
            summary: format!(
                "tracks {} → {}, markers {} → {}, cues {} → {}",
                a.0, b.0, a.1, b.1, a.2, b.2
            ),
        });
    }
    if old.ai != new.ai {
        let (a, b) = (&old.ai, &new.ai);
        let mut what = Vec::new();
        if a.commercial_safe_only != b.commercial_safe_only {
            what.push(format!(
                "commercial_safe_only {} → {}",
                a.commercial_safe_only, b.commercial_safe_only
            ));
        }
        if a.sovereign != b.sovereign {
            what.push(format!("sovereign {} → {}", a.sovereign, b.sovereign));
        }
        if a.spend_cap_usd != b.spend_cap_usd {
            what.push(format!(
                "spend_cap_usd {} → {}",
                a.spend_cap_usd, b.spend_cap_usd
            ));
        }
        if a.hosts != b.hosts {
            what.push(format!("hosts {} → {}", a.hosts.len(), b.hosts.len()));
        }
        d.sections.push(SectionChange {
            section: Section::Ai,
            presence: Presence::Changed,
            summary: what.join(", "),
        });
    }
    keyed(
        &mut d.sections,
        &old.models,
        &new.models,
        Section::Model,
        |a, b| {
            if a.sha256 != b.sha256 {
                format!("sha256 {} → {}", short(&a.sha256), short(&b.sha256))
            } else {
                format!(
                    "licence {} → {}",
                    a.licence_class.as_str(),
                    b.licence_class.as_str()
                )
            }
        },
    );
    d.sections.sort_by(|a, b| a.section.cmp(&b.section));
    d
}

/// Sections keyed by name: added, removed, or changed with `summary`.
fn keyed<T: PartialEq>(
    out: &mut Vec<SectionChange>,
    old: &BTreeMap<String, T>,
    new: &BTreeMap<String, T>,
    section: impl Fn(String) -> Section,
    summary: impl Fn(&T, &T) -> String,
) {
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for key in keys {
        let (presence, text) = match (old.get(key), new.get(key)) {
            (None, Some(_)) => (Presence::Added, String::new()),
            (Some(_), None) => (Presence::Removed, String::new()),
            (Some(a), Some(b)) if a != b => (Presence::Changed, summary(a, b)),
            _ => continue,
        };
        out.push(SectionChange {
            section: section(key.clone()),
            presence,
            summary: text,
        });
    }
}

/// The node and comment changes from `old` to `new`.
pub fn diff_networks(old: &Network, new: &Network) -> (Vec<NodeDiff>, Vec<CommentDiff>) {
    let ids: BTreeSet<NodeId> = old.nodes().chain(new.nodes()).map(|n| n.id).collect();
    let mut nodes = Vec::new();
    for id in ids {
        let (a, b) = (old.node(id), new.node(id));
        let shown = b.or(a).expect("the node is on one side");
        let presence = match (a, b) {
            (None, _) => Presence::Added,
            (_, None) => Presence::Removed,
            _ => Presence::Changed,
        };
        let mut changes = Vec::new();
        if let (Some(a), Some(b)) = (a, b) {
            if a.name != b.name {
                changes.push(NodeChange::Renamed {
                    old: a.name.clone(),
                    new: b.name.clone(),
                });
            }
            if a.type_id != b.type_id {
                changes.push(NodeChange::Retyped {
                    old: a.type_id.clone(),
                    new: b.type_id.clone(),
                });
            }
            if a.pos != b.pos {
                changes.push(NodeChange::Moved {
                    old: a.pos,
                    new: b.pos,
                });
            }
        }
        params(a, b, &mut changes);
        let inputs = |net: &Network, node: Option<&Node>| -> BTreeMap<String, Endpoint> {
            node.map(|n| {
                net.inputs_of(n.id)
                    .map(|(s, e)| (s.to_owned(), e.clone()))
                    .collect()
            })
            .unwrap_or_default()
        };
        let (ia, ib) = (inputs(old, a), inputs(new, b));
        let sockets: BTreeSet<&String> = ia.keys().chain(ib.keys()).collect();
        for socket in sockets {
            let (x, y) = (ia.get(socket), ib.get(socket));
            if x != y {
                changes.push(NodeChange::Input {
                    socket: socket.clone(),
                    old: x.cloned(),
                    new: y.cloned(),
                });
            }
        }
        if presence != Presence::Changed || !changes.is_empty() {
            nodes.push(NodeDiff {
                id,
                name: shown.name.clone(),
                type_id: shown.type_id.clone(),
                presence,
                changes,
            });
        }
    }

    let ids: BTreeSet<CommentId> = old.comments().chain(new.comments()).map(|c| c.id).collect();
    let mut comments = Vec::new();
    for id in ids {
        let (a, b) = (old.comment(id), new.comment(id));
        let presence = match (a, b) {
            (None, _) => Presence::Added,
            (_, None) => Presence::Removed,
            (Some(x), Some(y)) if x == y => continue,
            _ => Presence::Changed,
        };
        comments.push(CommentDiff {
            id,
            presence,
            old: a.cloned(),
            new: b.cloned(),
        });
    }
    (nodes, comments)
}

/// Edits that turn `old` into `new`, in an order [`Network::apply`]
/// accepts when both are valid networks, so a merge or a revision can be
/// applied as one undo step. A retyped node is removed and added again
/// under its ID. (`next_id` is not an edit: the result may keep a higher
/// counter than `new`.)
pub fn edits_between(old: &Network, new: &Network) -> Vec<Edit> {
    let mut edits = Vec::new();
    let replaced = |id: NodeId| match (old.node(id), new.node(id)) {
        (Some(a), Some(b)) => a.type_id != b.type_id,
        _ => true,
    };
    let old_wires: BTreeMap<Endpoint, Endpoint> = old.wires().map(|w| (w.to, w.from)).collect();
    let new_wires: BTreeMap<Endpoint, Endpoint> = new.wires().map(|w| (w.to, w.from)).collect();
    // Wires that change or go first, so no input is ever doubly fed.
    for (to, from) in &old_wires {
        if new_wires.get(to) != Some(from) && !replaced(to.node) && !replaced(from.node) {
            edits.push(Edit::Disconnect {
                wire: Wire::new(from.clone(), to.clone()),
            });
        }
    }
    for c in old.comments() {
        if new.comment(c.id).is_none() {
            edits.push(Edit::RemoveComment { id: c.id });
        }
    }
    for n in old.nodes() {
        if replaced(n.id) {
            edits.push(Edit::RemoveNode { id: n.id });
        }
    }
    // Renames go through a temporary name, so swapping two names works.
    let renamed: Vec<(&Node, &Node)> = old
        .nodes()
        .filter_map(|a| new.node(a.id).map(|b| (a, b)))
        .filter(|(a, b)| a.type_id == b.type_id && a.name != b.name)
        .collect();
    for (a, _) in &renamed {
        edits.push(Edit::Rename {
            node: a.id,
            name: format!("merge_tmp_{}", a.id.0),
        });
    }
    // Bindings may point at nodes added later: bind after all exist.
    let mut bind = Vec::new();
    for n in new.nodes() {
        if old.node(n.id).is_none() || replaced(n.id) {
            let mut node = n.clone();
            for (name, p) in &mut node.params {
                if p.bind.is_some() {
                    bind.push(Edit::SetParam {
                        node: n.id,
                        param: name.clone(),
                        value: Some(p.clone()),
                    });
                }
            }
            node.params.retain(|_, p| p.bind.is_none());
            edits.push(Edit::AddNode { node });
        }
    }
    for (_, b) in &renamed {
        edits.push(Edit::Rename {
            node: b.id,
            name: b.name.clone(),
        });
    }
    for b in new.nodes() {
        let Some(a) = old.node(b.id).filter(|a| a.type_id == b.type_id) else {
            continue;
        };
        if a.pos != b.pos {
            edits.push(Edit::Move {
                node: b.id,
                pos: b.pos,
            });
        }
        let mut changes = Vec::new();
        params(Some(a), Some(b), &mut changes);
        for c in changes {
            if let NodeChange::Param { name, new, .. } = c {
                edits.push(Edit::SetParam {
                    node: b.id,
                    param: name,
                    value: new,
                });
            }
        }
    }
    edits.extend(bind);
    for (to, from) in &new_wires {
        let kept = old_wires.get(to) == Some(from) && !replaced(to.node) && !replaced(from.node);
        if !kept {
            edits.push(Edit::Connect {
                wire: Wire::new(from.clone(), to.clone()),
            });
        }
    }
    for c in new.comments() {
        match old.comment(c.id) {
            None => edits.push(Edit::AddComment { comment: c.clone() }),
            Some(o) if o != c => edits.push(Edit::UpdateComment { comment: c.clone() }),
            _ => {}
        }
    }
    edits
}

fn params(a: Option<&Node>, b: Option<&Node>, out: &mut Vec<NodeChange>) {
    let empty = BTreeMap::new();
    let (pa, pb) = (
        a.map_or(&empty, |n| &n.params),
        b.map_or(&empty, |n| &n.params),
    );
    let names: BTreeSet<&String> = pa.keys().chain(pb.keys()).collect();
    for name in names {
        let (x, y) = (pa.get(name), pb.get(name));
        if x != y {
            out.push(NodeChange::Param {
                name: name.clone(),
                old: x.cloned(),
                new: y.cloned(),
            });
        }
    }
}

/// A parameter as it is written in a network file: `1.5`, or
/// `{ value = 1.0, expr = "t" }`; `default` for none.
pub fn param_text(param: Option<&Param>) -> String {
    let Some(p) = param else {
        return "default".into();
    };
    serde_json::to_value(p)
        .ok()
        .and_then(|j| network::param_to_toml(&j).ok())
        .map_or_else(
            || format!("{:?}", p.value),
            |v| v.to_string().trim().to_owned(),
        )
}

/// An input's source as `n3.out`; `(none)` when unconnected.
pub fn endpoint_text(e: Option<&Endpoint>) -> String {
    e.map_or_else(|| "(none)".into(), |e| format!("{}.{}", e.node, e.socket))
}

/// The pieces of a project's text a section is written as, for showing
/// two sides of a section.
pub fn section_text(project: &Project, section: &Section) -> Option<String> {
    match section {
        Section::Pack(name) => project
            .packs
            .get(name)
            .and_then(|def| pack_to_string(def).ok()),
        Section::Mappings => mappings_to_string(&project.mappings).ok(),
        Section::Timeline => project
            .timeline
            .as_ref()
            .and_then(|t| timeline_to_string(t).ok()),
        Section::Ai => Some(crate::merge::ai_text(&project.ai)),
        Section::Model(id) => project
            .models
            .get(id)
            .map(|m| format!("{} {}", m.licence_class.as_str(), m.sha256)),
    }
}

fn quoted(s: &str) -> String {
    format!("{s:?}")
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn pos_text(p: [f32; 2]) -> String {
    format!("[{:?}, {:?}]", p[0], p[1])
}

impl fmt::Display for NodeChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renamed { old, new } => write!(f, "renamed {old} → {new}"),
            Self::Retyped { old, new } => write!(f, "type {old} → {new}"),
            Self::Moved { old, new } => write!(f, "moved {} → {}", pos_text(*old), pos_text(*new)),
            Self::Param { name, old, new } => write!(
                f,
                "param {name}: {} → {}",
                param_text(old.as_ref()),
                param_text(new.as_ref())
            ),
            Self::Input { socket, old, new } => write!(
                f,
                "input {socket}: {} → {}",
                endpoint_text(old.as_ref()),
                endpoint_text(new.as_ref())
            ),
        }
    }
}

impl fmt::Display for ProjectDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return writeln!(f, "no changes");
        }
        for s in &self.settings {
            writeln!(f, "~ {}: {} → {}", s.key, s.old, s.new)?;
        }
        for net in &self.networks {
            writeln!(f, "{} network {}", net.presence.sign(), net.id)?;
            for node in &net.nodes {
                writeln!(
                    f,
                    "  {} {} {} ({})",
                    node.presence.sign(),
                    node.id,
                    node.name,
                    node.type_id
                )?;
                for c in &node.changes {
                    writeln!(f, "      {c}")?;
                }
            }
            for c in &net.comments {
                let text = c.new.as_ref().or(c.old.as_ref()).map_or("", |c| &c.text);
                let first = text.lines().next().unwrap_or("");
                writeln!(f, "  {} comment {} {first:?}", c.presence.sign(), c.id)?;
            }
        }
        for s in &self.sections {
            write!(f, "{} {}", s.presence.sign(), s.section)?;
            if s.summary.is_empty() {
                writeln!(f)?;
            } else {
                writeln!(f, ": {}", s.summary)?;
            }
        }
        Ok(())
    }
}
