//! Nested networks and Packs (FR-P4, PRD §5.5, `docs/packs.md`).
//!
//! A **Pack** is a network saved as a reusable unit ([`PackDef`]): a name, a
//! semantic [`Version`], embedded help, promoted parameters and input and
//! output sockets by wire type. A project-local nested network is a Pack
//! that is not locked. Placed in a network, a Pack is an ordinary node of
//! type `cmp.pack_<name>` whose parameters are the promoted parameters, so
//! wiring, type checks, bindings, expressions, undo and the project file
//! all work exactly as for built-in operators.
//!
//! - **Schema:** [`PackSet::register`] checks every definition (its inner
//!   network against the registry, promoted parameters against their
//!   targets, sockets, nesting cycles and depth) and adds one
//!   [`OperatorSchema`] per Pack, in dependency order.
//! - **Cooking:** [`PackSet::flatten`] expands every instance, recursively,
//!   into the nodes inside it, giving one flat network the engine cooks as
//!   usual. Flattening runs when a network is loaded or edited, never per
//!   frame, so the cook stays allocation-free; inner nodes get IDs derived
//!   from their instance path, so they are stable across edits and the cook
//!   order is deterministic.
//! - **Lock/unlock:** instances of a locked Pack cannot be edited inside
//!   ([`PackSet::edit`]); [`PackSet::unlock`] gives one instance its own
//!   editable copy.
//! - **Update in place:** [`PackSet::update`] replaces a definition across
//!   every instance, with a report of promoted-parameter changes and of what
//!   happened to each instance's values (see [`fit_instance`] for the rule).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde_json::Value;

use crate::graph::{Edit, Endpoint, GraphError, Network, Node, NodeId, Param, Wire};
use crate::history::transact;
use crate::registry::{
    OpFamily, OperatorSchema, ParamKind, ParamSchema, Registry, SocketSchema, WireType,
};

/// Type ID prefix of Pack instances: `cmp.pack_<name>` (PRD §6, CMP).
pub const TYPE_PREFIX: &str = "cmp.pack_";

/// Packs may contain Packs up to this depth; deeper nesting is an error.
pub const MAX_DEPTH: usize = 16;

/// Longest Pack, parameter or socket name.
pub const MAX_NAME_LEN: usize = 48;

/// Flat inner nodes get IDs with this bit set; top-level IDs stay below it.
const INNER_ID_BIT: u64 = 1 << 62;

/// Separates an instance's name from an inner node's name in flat names.
pub const SCOPE_SEP: &str = "__";

/// The operator type ID of Pack `name`.
pub fn type_id(name: &str) -> String {
    format!("{TYPE_PREFIX}{name}")
}

/// The Pack name of a Pack instance's type ID.
pub fn pack_name(type_id: &str) -> Option<&str> {
    type_id.strip_prefix(TYPE_PREFIX)
}

/// A Pack name: lower-case letters, digits and `_`, starting with a letter.
pub fn valid_pack_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LEN
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A parameter or socket name: like a Pack name, but may start with `_`.
fn valid_member_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A semantic version, `MAJOR.MINOR.PATCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// True if `newer` may replace `self` without breaking instances
    /// (semantic versioning: same major, not older; for 0.x, same minor).
    pub fn compatible_with(self, newer: Version) -> bool {
        VersionReq::Compatible(self).matches(newer)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn version_part(s: &str) -> Option<u32> {
    let ok = !s.is_empty()
        && s.len() <= 9
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    ok.then(|| s.parse().ok()).flatten()
}

impl FromStr for Version {
    type Err = PackError;

    fn from_str(s: &str) -> Result<Self, PackError> {
        let parts: Vec<&str> = s.split('.').collect();
        match parts[..] {
            [a, b, c] => match (version_part(a), version_part(b), version_part(c)) {
                (Some(major), Some(minor), Some(patch)) => Ok(Self::new(major, minor, patch)),
                _ => Err(PackError::InvalidVersion(s.to_owned())),
            },
            _ => Err(PackError::InvalidVersion(s.to_owned())),
        }
    }
}

/// Which versions of a Pack a request accepts: `*` (any), `1.2.3`
/// (exactly), `^1.2.3`, `^1.2` or `^1` (compatible, as in Cargo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionReq {
    Any,
    Exact(Version),
    Compatible(Version),
}

impl VersionReq {
    pub fn matches(self, v: Version) -> bool {
        match self {
            VersionReq::Any => true,
            VersionReq::Exact(e) => v == e,
            VersionReq::Compatible(b) => {
                v >= b
                    && if b.major > 0 {
                        v.major == b.major
                    } else if b.minor > 0 {
                        v.major == 0 && v.minor == b.minor
                    } else {
                        v == b
                    }
            }
        }
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionReq::Any => f.write_str("*"),
            VersionReq::Exact(v) => write!(f, "{v}"),
            VersionReq::Compatible(v) => write!(f, "^{v}"),
        }
    }
}

impl FromStr for VersionReq {
    type Err = PackError;

    fn from_str(s: &str) -> Result<Self, PackError> {
        let s = s.trim();
        if s.is_empty() || s == "*" {
            return Ok(VersionReq::Any);
        }
        let Some(rest) = s.strip_prefix('^') else {
            return Ok(VersionReq::Exact(s.parse()?));
        };
        let parts: Vec<Option<u32>> = rest.split('.').map(version_part).collect();
        let bad = || PackError::InvalidVersion(s.to_owned());
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(Option::is_none) {
            return Err(bad());
        }
        let p = |i: usize| parts.get(i).copied().flatten().unwrap_or(0);
        Ok(VersionReq::Compatible(Version::new(p(0), p(1), p(2))))
    }
}

/// A Pack and version, written `name@1.2.3`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackRef {
    pub name: String,
    pub version: Version,
}

impl fmt::Display for PackRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

impl FromStr for PackRef {
    type Err = PackError;

    fn from_str(s: &str) -> Result<Self, PackError> {
        let (name, version) = s
            .split_once('@')
            .ok_or_else(|| PackError::InvalidVersion(s.to_owned()))?;
        if !valid_pack_name(name) {
            return Err(PackError::InvalidName(name.to_owned()));
        }
        Ok(Self {
            name: name.to_owned(),
            version: version.parse()?,
        })
    }
}

/// One parameter inside a Pack that a promoted parameter drives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParamTarget {
    pub node: NodeId,
    pub param: String,
}

impl ParamTarget {
    pub fn new(node: NodeId, param: impl Into<String>) -> Self {
        Self {
            node,
            param: param.into(),
        }
    }
}

/// A parameter of the Pack itself. Its kind is the kind of its targets
/// (all targets share one kind); `min`/`max` may narrow their range, and the
/// label and help default to the first target's.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotedParam {
    pub name: String,
    /// Empty: the first target's label.
    pub label: String,
    pub default: Value,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Empty: the first target's help.
    pub help: String,
    /// The inner parameters it sets. An instance's value (constant,
    /// binding or expression) replaces each target's own.
    pub targets: Vec<ParamTarget>,
}

/// An input socket of a Pack: what arrives on it feeds these inner inputs
/// (which must not be wired inside the Pack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackInput {
    pub name: String,
    pub targets: Vec<Endpoint>,
}

/// An output socket of a Pack: the inner output it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOutput {
    pub name: String,
    pub source: Endpoint,
}

/// A Pack definition: a network with an interface, a version and help.
#[derive(Debug, Clone, PartialEq)]
pub struct PackDef {
    /// Lower-case identifier; the operator type is `cmp.pack_<name>`.
    pub name: String,
    pub version: Version,
    /// Display name (empty: the name).
    pub label: String,
    /// One sentence for the Palette and tooltips.
    pub summary: String,
    /// Embedded help (Markdown), shown in the app and written to `help.md`.
    pub help: String,
    /// A locked Pack cannot be edited inside; unlocking an instance gives
    /// it an editable copy.
    pub locked: bool,
    /// For an unlocked copy: the Pack and version it was copied from.
    pub derived_from: Option<PackRef>,
    pub params: Vec<PromotedParam>,
    pub inputs: Vec<PackInput>,
    pub outputs: Vec<PackOutput>,
    pub network: Network,
}

impl PackDef {
    /// An empty, unlocked Pack at version 0.1.0.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: Version::new(0, 1, 0),
            label: String::new(),
            summary: String::new(),
            help: String::new(),
            locked: false,
            derived_from: None,
            params: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            network: Network::default(),
        }
    }

    pub fn type_id(&self) -> String {
        type_id(&self.name)
    }

    pub fn reference(&self) -> PackRef {
        PackRef {
            name: self.name.clone(),
            version: self.version,
        }
    }

    /// The label shown in the UI.
    pub fn display_label(&self) -> &str {
        if self.label.is_empty() {
            &self.name
        } else {
            &self.label
        }
    }

    /// Names of the Packs this one contains directly, sorted.
    pub fn dependencies(&self) -> BTreeSet<&str> {
        self.network
            .nodes()
            .filter_map(|n| pack_name(&n.type_id))
            .collect()
    }

    /// Checks this definition against `registry` (which must hold every
    /// operator and Pack it contains) and returns its operator schema.
    pub fn schema(&self, registry: &Registry) -> Result<OperatorSchema, PackError> {
        let err = |message: String| PackError::Invalid {
            pack: self.name.clone(),
            message,
        };
        if !valid_pack_name(&self.name) {
            return Err(PackError::InvalidName(self.name.clone()));
        }
        let net = self
            .network
            .clone()
            .validated(registry)
            .map_err(|source| PackError::Graph {
                pack: self.name.clone(),
                source,
            })?;
        let node_schema = |id: NodeId, what: &str| {
            let node = net.node(id).ok_or_else(|| {
                err(format!(
                    "{what} refers to node {id}, which is not in the Pack"
                ))
            })?;
            registry
                .get(&node.type_id)
                .ok_or_else(|| err(format!("unknown operator `{}`", node.type_id)))
        };

        // Promoted parameters.
        let mut params = Vec::new();
        let mut targeted = BTreeSet::new();
        for p in &self.params {
            let what = format!("parameter `{}`", p.name);
            if !valid_member_name(&p.name) {
                return Err(err(format!(
                    "{what}: use a-z, 0-9 and `_` (at most {MAX_NAME_LEN} characters)"
                )));
            }
            if params.iter().any(|q: &ParamSchema| q.name == p.name) {
                return Err(err(format!("{what} is declared twice")));
            }
            if p.targets.is_empty() {
                return Err(err(format!("{what} drives no inner parameter")));
            }
            let mut targets = Vec::new();
            for t in &p.targets {
                let schema = node_schema(t.node, &what)?;
                let ps = schema
                    .params
                    .iter()
                    .find(|q| q.name == t.param)
                    .ok_or_else(|| {
                        err(format!(
                            "{what}: `{}` has no parameter `{}`",
                            schema.type_id, t.param
                        ))
                    })?;
                if !targeted.insert(t.clone()) {
                    return Err(err(format!(
                        "{what}: {}.{} is driven by two promoted parameters",
                        t.node, t.param
                    )));
                }
                targets.push(ps);
            }
            let first = targets[0];
            if let Some(other) = targets.iter().find(|t| !same_kind(&t.kind, &first.kind)) {
                return Err(err(format!(
                    "{what}: its targets differ in kind (`{}` and `{}`)",
                    first.name, other.name
                )));
            }
            let kind = narrow(&first.kind, p.min, p.max, &targets)
                .map_err(|m| err(format!("{what}: {m}")))?;
            let schema = ParamSchema {
                name: p.name.clone(),
                label: if p.label.is_empty() {
                    first.label.clone()
                } else {
                    p.label.clone()
                },
                kind,
                default: p.default.clone(),
                help: if p.help.is_empty() {
                    first.help.clone()
                } else {
                    p.help.clone()
                },
                suggest: first.suggest.clone(),
                multiline: first.multiline,
            };
            schema
                .validate(&p.default)
                .map_err(|m| err(format!("{what}: default: {m}")))?;
            params.push(schema);
        }

        // Outputs first: inputs need them to decide which are feedback.
        let mut outputs: Vec<SocketSchema> = Vec::new();
        for o in &self.outputs {
            let what = format!("output `{}`", o.name);
            if !valid_member_name(&o.name) {
                return Err(err(format!("{what}: use a-z, 0-9 and `_`")));
            }
            if outputs.iter().any(|s| s.name == o.name) {
                return Err(err(format!("{what} is declared twice")));
            }
            let schema = node_schema(o.source.node, &what)?;
            let socket = schema
                .outputs
                .iter()
                .find(|s| s.name == o.source.socket)
                .ok_or_else(|| {
                    err(format!(
                        "{what}: `{}` has no output `{}`",
                        schema.type_id, o.source.socket
                    ))
                })?;
            outputs.push(SocketSchema {
                name: o.name.clone(),
                wire: socket.wire,
                optional: false,
                feedback: false,
            });
        }
        let sources: BTreeSet<NodeId> = self.outputs.iter().map(|o| o.source.node).collect();

        let mut inputs: Vec<SocketSchema> = Vec::new();
        let mut fed = BTreeSet::new();
        for i in &self.inputs {
            let what = format!("input `{}`", i.name);
            if !valid_member_name(&i.name) {
                return Err(err(format!("{what}: use a-z, 0-9 and `_`")));
            }
            if inputs.iter().any(|s| s.name == i.name) {
                return Err(err(format!("{what} is declared twice")));
            }
            if i.targets.is_empty() {
                return Err(err(format!("{what} feeds no inner input")));
            }
            let mut wire: Option<WireType> = None;
            let mut feedback = true;
            for t in &i.targets {
                let schema = node_schema(t.node, &what)?;
                let socket = schema
                    .inputs
                    .iter()
                    .find(|s| s.name == t.socket)
                    .ok_or_else(|| {
                        err(format!(
                            "{what}: `{}` has no input `{}`",
                            schema.type_id, t.socket
                        ))
                    })?;
                if net.inputs_of(t.node).any(|(s, _)| s == t.socket) {
                    return Err(err(format!(
                        "{what} feeds {}.{}, which is already wired inside the Pack",
                        t.node, t.socket
                    )));
                }
                if !fed.insert(t.clone()) {
                    return Err(err(format!(
                        "{}.{} is fed by two Pack inputs",
                        t.node, t.socket
                    )));
                }
                match wire {
                    Some(w) if w != socket.wire => {
                        return Err(err(format!(
                            "{what} feeds inputs of different wire types ({w:?} and {:?})",
                            socket.wire
                        )));
                    }
                    _ => wire = Some(socket.wire),
                }
                if !socket.feedback && reaches_any(&net, registry, t.node, &sources) {
                    feedback = false;
                }
            }
            inputs.push(SocketSchema {
                name: i.name.clone(),
                wire: wire.expect("at least one target"),
                optional: true,
                feedback,
            });
        }

        let mut capabilities = Vec::new();
        let mut budget = 0u32;
        let (mut time_dependent, mut sink) = (false, false);
        for node in net.nodes() {
            let s = registry.get(&node.type_id).expect("validated");
            for c in &s.capabilities {
                if !capabilities.contains(c) {
                    capabilities.push(c.clone());
                }
            }
            budget = budget.saturating_add(s.cook_budget_us);
            time_dependent |= s.time_dependent;
            sink |= s.sink;
        }
        Ok(OperatorSchema {
            type_id: self.type_id(),
            family: OpFamily::Cmp,
            label: self.display_label().to_owned(),
            summary: self.summary.clone(),
            version: self.version.major,
            params,
            inputs,
            outputs,
            capabilities,
            cook_budget_us: budget,
            time_dependent,
            sink,
        })
    }
}

/// Same parameter kind, ignoring numeric ranges.
fn same_kind(a: &ParamKind, b: &ParamKind) -> bool {
    match (a, b) {
        (ParamKind::Float { .. }, ParamKind::Float { .. })
        | (ParamKind::Int { .. }, ParamKind::Int { .. }) => true,
        _ => a == b,
    }
}

/// The promoted kind: the targets' kind with the tightest range they all
/// accept, optionally narrowed further by `min`/`max`.
fn narrow(
    kind: &ParamKind,
    min: Option<f64>,
    max: Option<f64>,
    targets: &[&ParamSchema],
) -> Result<ParamKind, String> {
    // The range every target accepts.
    let mut lo: Option<f64> = None;
    let mut hi: Option<f64> = None;
    for t in targets {
        let (tmin, tmax) = match t.kind {
            ParamKind::Float { min, max } => (min, max),
            ParamKind::Int { min, max } => (min.map(|v| v as f64), max.map(|v| v as f64)),
            _ => (None, None),
        };
        lo = match (lo, tmin) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        hi = match (hi, tmax) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    let pick =
        |own: Option<f64>, target: Option<f64>, lower: bool| -> Result<Option<f64>, String> {
            match (own, target) {
                (Some(v), _) if !v.is_finite() => Err(format!("{v} is not a finite limit")),
                (Some(v), Some(t)) if (lower && v < t) || (!lower && v > t) => Err(format!(
                    "{} {v} is outside what the inner parameter accepts ({t})",
                    if lower { "min" } else { "max" }
                )),
                (Some(v), _) => Ok(Some(v)),
                (None, t) => Ok(t),
            }
        };
    match kind {
        ParamKind::Float { .. } => {
            let (min, max) = (pick(min, lo, true)?, pick(max, hi, false)?);
            if let (Some(a), Some(b)) = (min, max)
                && a > b
            {
                return Err(format!("min {a} is above max {b}"));
            }
            Ok(ParamKind::Float { min, max })
        }
        ParamKind::Int { .. } => {
            let int = |v: Option<f64>| -> Result<Option<i64>, String> {
                match v {
                    Some(f) if f.fract() != 0.0 || f.abs() > 9.0e15 => {
                        Err(format!("{f} is not a whole-number limit"))
                    }
                    Some(f) => Ok(Some(f as i64)),
                    None => Ok(None),
                }
            };
            let (min, max) = (int(pick(min, lo, true)?)?, int(pick(max, hi, false)?)?);
            if let (Some(a), Some(b)) = (min, max)
                && a > b
            {
                return Err(format!("min {a} is above max {b}"));
            }
            Ok(ParamKind::Int { min, max })
        }
        other => {
            if min.is_some() || max.is_some() {
                return Err("only number parameters have min and max".into());
            }
            Ok(other.clone())
        }
    }
}

/// True if a node in `targets` is reachable from `from` (inclusive)
/// following wires and channel bindings forward, ignoring wires into
/// feedback inputs.
fn reaches_any(
    net: &Network,
    registry: &Registry,
    from: NodeId,
    targets: &BTreeSet<NodeId>,
) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(n) = stack.pop() {
        if targets.contains(&n) {
            return true;
        }
        if !seen.insert(n) {
            continue;
        }
        for w in net.wires().filter(|w| w.from.node == n) {
            let feedback = net
                .node(w.to.node)
                .and_then(|node| registry.get(&node.type_id))
                .and_then(|s| s.inputs.iter().find(|i| i.name == w.to.socket))
                .is_some_and(|s| s.feedback);
            if !feedback {
                stack.push(w.to.node);
            }
        }
        for other in net.nodes() {
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

/// Errors about Packs. Each names the Pack it concerns.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PackError {
    #[error(
        "invalid Pack name `{0}`: use a-z, 0-9 and `_`, starting with a letter, at most {MAX_NAME_LEN} characters"
    )]
    InvalidName(String),
    #[error("invalid version `{0}`: write MAJOR.MINOR.PATCH, e.g. 1.0.0")]
    InvalidVersion(String),
    #[error("unknown Pack `{0}`")]
    Unknown(String),
    #[error("Packs contain each other: {0}")]
    Cycle(String),
    #[error("Packs nest more than {MAX_DEPTH} deep: {0}")]
    TooDeep(String),
    #[error("Pack `{pack}`: {message}")]
    Invalid { pack: String, message: String },
    #[error("Pack `{pack}`, inner network: {source}")]
    Graph { pack: String, source: GraphError },
    #[error("Pack `{0}` is locked: unlock the instance to edit its own copy")]
    Locked(String),
    #[error("{0}")]
    Network(String),
}

/// The Pack definitions of a project, by name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackSet {
    defs: BTreeMap<String, PackDef>,
}

impl PackSet {
    /// Adds or replaces a definition, returning the one it replaced.
    pub fn insert(&mut self, def: PackDef) -> Option<PackDef> {
        self.defs.insert(def.name.clone(), def)
    }

    pub fn remove(&mut self, name: &str) -> Option<PackDef> {
        self.defs.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&PackDef> {
        self.defs.get(name)
    }

    /// The definition behind an operator type ID (`cmp.pack_<name>`).
    pub fn for_type(&self, type_id: &str) -> Option<&PackDef> {
        pack_name(type_id).and_then(|n| self.defs.get(n))
    }

    /// Definitions in name order.
    pub fn iter(&self) -> impl Iterator<Item = &PackDef> {
        self.defs.values()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// True if nodes of `type_id` are instances of a locked Pack.
    pub fn is_locked(&self, type_id: &str) -> bool {
        self.for_type(type_id).is_some_and(|d| d.locked)
    }

    /// Pack names in dependency order (each after the Packs it contains).
    /// Fails on a Pack that contains itself (directly or not), on nesting
    /// deeper than [`MAX_DEPTH`] and on unknown Packs.
    pub fn order(&self) -> Result<Vec<&str>, PackError> {
        let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
        let mut order = Vec::new();
        let mut path = Vec::new();
        for name in self.defs.keys() {
            self.visit(name, &mut path, &mut depth, &mut order)?;
        }
        Ok(order)
    }

    /// Depth-first visit; recursion is bounded by [`MAX_DEPTH`].
    fn visit<'a>(
        &'a self,
        name: &'a str,
        path: &mut Vec<&'a str>,
        depth: &mut BTreeMap<&'a str, usize>,
        order: &mut Vec<&'a str>,
    ) -> Result<usize, PackError> {
        if let Some(i) = path.iter().position(|p| *p == name) {
            let mut cycle: Vec<&str> = path[i..].to_vec();
            cycle.push(name);
            return Err(PackError::Cycle(cycle.join(" → ")));
        }
        let chain = |path: &[&str]| {
            let mut c = path.to_vec();
            c.push(name);
            c.join(" → ")
        };
        if let Some(&d) = depth.get(name) {
            if path.len() + d > MAX_DEPTH {
                return Err(PackError::TooDeep(chain(path) + " → …"));
            }
            return Ok(d);
        }
        if path.len() >= MAX_DEPTH {
            return Err(PackError::TooDeep(chain(path)));
        }
        let def = self
            .defs
            .get(name)
            .ok_or_else(|| PackError::Unknown(name.into()))?;
        path.push(name);
        let mut d = 1;
        for dep in def.dependencies() {
            if !self.defs.contains_key(dep) {
                path.pop();
                return Err(PackError::Invalid {
                    pack: name.into(),
                    message: format!("contains unknown Pack `{dep}`"),
                });
            }
            let dep = self.defs.get_key_value(dep).expect("checked").0.as_str();
            match self.visit(dep, path, depth, order) {
                Ok(dd) => d = d.max(dd + 1),
                Err(e) => {
                    path.pop();
                    return Err(e);
                }
            }
        }
        path.pop();
        depth.insert(name, d);
        order.push(name);
        Ok(d)
    }

    /// `base` plus one operator schema per Pack, after checking every
    /// definition (see [`PackDef::schema`]).
    pub fn register(&self, base: &Registry) -> Result<Registry, PackError> {
        let mut reg = base.clone();
        for name in self.order()? {
            let schema = self.defs[name].schema(&reg)?;
            reg.register(schema).map_err(|e| PackError::Invalid {
                pack: name.into(),
                message: e.to_string(),
            })?;
        }
        Ok(reg)
    }

    /// Expands every Pack instance in `net` into the nodes inside it (see
    /// [`Flat`]). `registry` must hold the Pack schemas
    /// ([`Self::register`]); `net` should be valid against it.
    pub fn flatten(&self, net: &Network, registry: &Registry) -> Result<Flat, PackError> {
        let mut f = Flattener {
            packs: self,
            registry,
            locals: BTreeMap::new(),
            scopes: BTreeMap::new(),
            nodes: Vec::new(),
            deferred: Vec::new(),
            wires: Vec::new(),
            used: BTreeSet::new(),
            flat: Flat::default(),
        };
        f.walk(net, &[], "", "", &BTreeMap::new(), 0)?;
        f.finish(net)
    }

    /// Applies `edits` to the network inside Pack `name` (one undoable
    /// step; returns the inverse edits). Refused for a locked Pack, and
    /// rolled back if the Pack would no longer be valid (for example, a
    /// removed node that a promoted parameter drives).
    pub fn edit(
        &mut self,
        name: &str,
        base: &Registry,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<Vec<Edit>, PackError> {
        let def = self
            .defs
            .get(name)
            .ok_or_else(|| PackError::Unknown(name.into()))?;
        if def.locked {
            return Err(PackError::Locked(name.into()));
        }
        let reg = self.register(base)?;
        let mut net = def.network.clone();
        let (inverse, _) = transact(&mut net, &reg, edits).map_err(|source| PackError::Graph {
            pack: name.into(),
            source,
        })?;
        let slot = &mut self.defs.get_mut(name).expect("checked").network;
        let old = std::mem::replace(slot, net);
        if let Err(e) = self.register(base) {
            self.defs.get_mut(name).expect("checked").network = old;
            return Err(e);
        }
        Ok(inverse)
    }

    /// Replaces the interface or metadata of an unlocked Pack (promoted
    /// parameters, sockets, help, version), keeping the change only if every
    /// Pack is still valid. Returns the previous definition.
    pub fn redefine(&mut self, def: PackDef, base: &Registry) -> Result<PackDef, PackError> {
        let old = self
            .defs
            .get(&def.name)
            .ok_or_else(|| PackError::Unknown(def.name.clone()))?;
        if old.locked {
            return Err(PackError::Locked(def.name));
        }
        let old = self.insert(def).expect("checked");
        if let Err(e) = self.register(base) {
            self.insert(old);
            return Err(e);
        }
        Ok(old)
    }

    /// Gives instance `instance` of a locked Pack its own editable copy
    /// (`<name>_local<n>`, unlocked, remembering what it was copied from).
    /// Returns the copy's name and the edits that switch the instance to it;
    /// register the Packs again ([`Self::register`]) before applying them.
    pub fn unlock(
        &mut self,
        net: &Network,
        instance: NodeId,
    ) -> Result<(String, Vec<Edit>), PackError> {
        let node = net
            .node(instance)
            .ok_or_else(|| PackError::Network(format!("no node {instance}")))?;
        let def = self
            .for_type(&node.type_id)
            .ok_or_else(|| PackError::Network(format!("{} is not a Pack instance", node.name)))?;
        if !def.locked {
            return Err(PackError::Invalid {
                pack: def.name.clone(),
                message: "is not locked".into(),
            });
        }
        let mut base = def.name.clone();
        base.truncate(MAX_NAME_LEN - "_local999".len());
        let name = (1..)
            .map(|n| format!("{base}_local{n}"))
            .find(|n| !self.defs.contains_key(n))
            .expect("a free name exists");
        let mut copy = def.clone();
        copy.derived_from = Some(def.reference());
        copy.name = name.clone();
        copy.locked = false;
        self.insert(copy);
        let edits = retype(net, instance, &type_id(&name), node.params.clone(), None);
        Ok((name, edits))
    }

    /// Returns an unlocked copy's instance to the Pack it was copied from
    /// (the inner changes are discarded; instance values are kept by the
    /// rule of [`fit_instance`]). `registry` must hold both schemas.
    pub fn relock(
        &self,
        net: &Network,
        registry: &Registry,
        instance: NodeId,
    ) -> Result<(Vec<Edit>, Vec<String>), PackError> {
        let node = net
            .node(instance)
            .ok_or_else(|| PackError::Network(format!("no node {instance}")))?;
        let def = self
            .for_type(&node.type_id)
            .ok_or_else(|| PackError::Network(format!("{} is not a Pack instance", node.name)))?;
        let origin = def
            .derived_from
            .as_ref()
            .ok_or_else(|| PackError::Invalid {
                pack: def.name.clone(),
                message: "was not copied from another Pack".into(),
            })?;
        let target = self
            .get(&origin.name)
            .ok_or_else(|| PackError::Unknown(origin.name.clone()))?;
        let schema = |t: &str| {
            registry
                .get(t)
                .ok_or_else(|| PackError::Unknown(pack_name(t).unwrap_or(t).into()))
        };
        let (old, new) = (schema(&node.type_id)?, schema(&target.type_id())?);
        let fit = fit_instance(net, instance, old, new);
        let edits = retype(
            net,
            instance,
            &target.type_id(),
            fit.params.clone(),
            Some(&fit),
        );
        Ok((edits, fit.notes))
    }

    /// Replaces Pack `new.name` with `new` across the project: in
    /// `networks` and in every Pack that contains it. Instance values are
    /// kept, clamped or reset by the rule of [`fit_instance`]; the report
    /// lists the interface changes and what happened to each instance. On
    /// any error nothing changes. (Undo: keep a copy of the Packs and
    /// networks; this is one project-level step.)
    pub fn update(
        &mut self,
        new: PackDef,
        base: &Registry,
        networks: &mut BTreeMap<String, Network>,
    ) -> Result<UpdateReport, PackError> {
        let old_def = self
            .defs
            .get(&new.name)
            .ok_or_else(|| PackError::Unknown(new.name.clone()))?
            .clone();
        let old_reg = self.register(base)?;
        let old_schema = old_reg.get(&old_def.type_id()).expect("registered").clone();
        let new_schema = new.schema(&old_reg)?;
        let mut mixed = old_reg.clone();
        mixed
            .replace(new_schema.clone())
            .map_err(|e| PackError::Invalid {
                pack: new.name.clone(),
                message: e.to_string(),
            })?;
        let mut report = UpdateReport {
            pack: new.name.clone(),
            from: old_def.version,
            to: new.version,
            changes: interface_changes(&old_schema, &new_schema),
            instances: Vec::new(),
        };

        let saved_packs = self.clone();
        let saved_nets = networks.clone();
        let result = (|| {
            let ty = new.type_id();
            // Instances inside other Packs (locked or not: this is the
            // project's definition of the Pack).
            let holders: Vec<String> = self
                .defs
                .values()
                .filter(|d| d.name != new.name && d.network.nodes().any(|n| n.type_id == ty))
                .map(|d| d.name.clone())
                .collect();
            for holder in holders {
                let net = &mut self.defs.get_mut(&holder).expect("listed").network;
                let place = format!("pack {holder}");
                fit_all(
                    net,
                    &mixed,
                    &ty,
                    &old_schema,
                    &new_schema,
                    &place,
                    &mut report,
                )?;
            }
            for (id, net) in networks.iter_mut() {
                fit_all(net, &mixed, &ty, &old_schema, &new_schema, id, &mut report)?;
            }
            self.insert(new);
            let reg = self.register(base)?;
            for (id, net) in networks.iter() {
                net.clone()
                    .validated(&reg)
                    .map_err(|e| PackError::Network(format!("networks/{id}: {e}")))?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            *self = saved_packs;
            *networks = saved_nets;
            return Err(e);
        }
        Ok(report)
    }
}

/// Retypes every instance of `ty` in `net` from `old` to `new`.
fn fit_all(
    net: &mut Network,
    registry: &Registry,
    ty: &str,
    old: &OperatorSchema,
    new: &OperatorSchema,
    place: &str,
    report: &mut UpdateReport,
) -> Result<(), PackError> {
    let ids: Vec<NodeId> = net
        .nodes()
        .filter(|n| n.type_id == ty)
        .map(|n| n.id)
        .collect();
    for id in ids {
        let fit = fit_instance(net, id, old, new);
        let name = net.node(id).map(|n| n.name.clone()).unwrap_or_default();
        for note in &fit.notes {
            report.instances.push(format!("{place}/{name}: {note}"));
        }
        let edits = retype(net, id, ty, fit.params.clone(), Some(&fit));
        transact(net, registry, edits)
            .map_err(|e| PackError::Network(format!("{place}/{name}: {e}")))?;
    }
    Ok(())
}

/// What survives when an instance changes from one Pack interface to
/// another ([`fit_instance`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    /// The instance's parameters under the new interface.
    pub params: BTreeMap<String, Param>,
    /// Wires into or out of the instance that no longer fit.
    pub dropped_wires: Vec<Wire>,
    /// Other nodes' parameters bound to the instance that must lose their
    /// binding (the Pack has no SIG output any more).
    pub unbound: Vec<(NodeId, String)>,
    /// One human-readable line per change.
    pub notes: Vec<String>,
}

/// The safe rule for moving an instance from interface `old` to `new`:
///
/// - a value (constant, binding or expression) the new parameter accepts
///   is kept, unless it is a plain constant equal to the new default (then
///   the instance simply follows the default);
/// - a number outside the new range is clamped into it;
/// - anything else (the parameter disappeared, or changed kind) is reset to
///   the new default, and the old value is reported so it can be restored;
/// - wires to sockets that disappeared or changed wire type are removed;
///   bindings to the instance are removed if it has no SIG output any more.
pub fn fit_instance(
    net: &Network,
    instance: NodeId,
    old: &OperatorSchema,
    new: &OperatorSchema,
) -> Fit {
    let mut fit = Fit {
        params: BTreeMap::new(),
        dropped_wires: Vec::new(),
        unbound: Vec::new(),
        notes: Vec::new(),
    };
    let Some(node) = net.node(instance) else {
        return fit;
    };
    for (name, p) in &node.params {
        let shown = show_param(p);
        let Some(ps) = new.params.iter().find(|q| &q.name == name) else {
            fit.notes.push(format!(
                "`{name}` = {shown} dropped: the parameter was removed"
            ));
            continue;
        };
        let numeric = matches!(
            ps.kind,
            ParamKind::Float { .. } | ParamKind::Int { .. } | ParamKind::Toggle
        );
        let driven = p.bind.is_some() || p.expr.is_some();
        if driven && !numeric {
            fit.notes.push(format!(
                "`{name}` = {shown} reset: only number and toggle parameters follow channels or expressions"
            ));
            continue;
        }
        let mut kept = p.clone();
        if ps.validate(&p.value).is_err() {
            match clamp(&ps.kind, &p.value) {
                Some(v) if ps.validate(&v).is_ok() => {
                    fit.notes
                        .push(format!("`{name}`: {} clamped to {v}", p.value));
                    kept.value = v;
                }
                _ if driven => {
                    fit.notes.push(format!(
                        "`{name}`: fallback {} reset to the default {}",
                        p.value, ps.default
                    ));
                    kept.value = ps.default.clone();
                }
                _ => {
                    fit.notes.push(format!(
                        "`{name}` = {shown} reset to the default {}: the parameter changed kind",
                        ps.default
                    ));
                    continue;
                }
            }
        }
        if !driven && kept.value == ps.default {
            continue;
        }
        fit.params.insert(name.clone(), kept);
    }
    let fits = |list: &[SocketSchema], oldl: &[SocketSchema], name: &str| {
        let was = oldl.iter().find(|s| s.name == name).map(|s| s.wire);
        list.iter()
            .find(|s| s.name == name)
            .is_some_and(|s| was.is_none_or(|w| w == s.wire))
    };
    for w in net.wires() {
        if w.to.node == instance && !fits(&new.inputs, &old.inputs, &w.to.socket) {
            fit.notes.push(format!(
                "input `{}` disconnected: the socket was removed or changed type",
                w.to.socket
            ));
            fit.dropped_wires.push(w);
        } else if w.from.node == instance && !fits(&new.outputs, &old.outputs, &w.from.socket) {
            fit.notes.push(format!(
                "output `{}` disconnected: the socket was removed or changed type",
                w.from.socket
            ));
            fit.dropped_wires.push(w);
        }
    }
    if !new.outputs.iter().any(|o| o.wire == WireType::Sig) {
        for other in net.nodes() {
            for (pname, p) in &other.params {
                if p.bind.as_ref().is_some_and(|b| b.node == instance) {
                    fit.notes.push(format!(
                        "{}.{pname} no longer follows it: the Pack has no SIG output",
                        other.name
                    ));
                    fit.unbound.push((other.id, pname.clone()));
                }
            }
        }
    }
    fit
}

fn show_param(p: &Param) -> String {
    if let Some(e) = &p.expr {
        format!("expression `{e}`")
    } else if let Some(b) = &p.bind {
        format!("binding to {}:{}", b.node, b.channel)
    } else {
        p.value.to_string()
    }
}

/// Clamps a number into a numeric kind's range.
fn clamp(kind: &ParamKind, value: &Value) -> Option<Value> {
    match kind {
        ParamKind::Float { min, max } => {
            let v = value.as_f64()?;
            let v = min.map_or(v, |m| v.max(m));
            Some(Value::from(max.map_or(v, |m| v.min(m))))
        }
        ParamKind::Int { min, max } => {
            let v = value
                .as_i64()
                .or_else(|| value.as_f64().map(|f| f.round() as i64))?;
            let v = min.map_or(v, |m| v.max(m));
            Some(Value::from(max.map_or(v, |m| v.min(m))))
        }
        _ => None,
    }
}

/// Edits that change node `id` to type `new_type` with `params`, keeping
/// its wires (except `fit.dropped_wires`) and the bindings that point at it
/// (except `fit.unbound`).
fn retype(
    net: &Network,
    id: NodeId,
    new_type: &str,
    params: BTreeMap<String, Param>,
    fit: Option<&Fit>,
) -> Vec<Edit> {
    let Some(old) = net.node(id) else {
        return Vec::new();
    };
    let dropped = |w: &Wire| fit.is_some_and(|f| f.dropped_wires.contains(w));
    let wires: Vec<Wire> = net
        .wires()
        .filter(|w| (w.from.node == id || w.to.node == id) && !dropped(w))
        .collect();
    let mut node = old.clone();
    node.type_id = new_type.to_owned();
    node.params = params;
    let mut edits = vec![Edit::RemoveNode { id }, Edit::RestoreNode { node, wires }];
    for other in net.nodes() {
        for (pname, p) in &other.params {
            if p.bind.as_ref().is_some_and(|b| b.node == id) {
                let unbind = fit.is_some_and(|f| f.unbound.contains(&(other.id, pname.clone())));
                if !unbind {
                    edits.push(Edit::SetParam {
                        node: other.id,
                        param: pname.clone(),
                        value: Some(p.clone()),
                    });
                }
            }
        }
    }
    edits
}

/// One change to a Pack's interface between two versions.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    ParamAdded {
        name: String,
    },
    ParamRemoved {
        name: String,
    },
    ParamKind {
        name: String,
        from: String,
        to: String,
    },
    ParamDefault {
        name: String,
        from: Value,
        to: Value,
    },
    InputAdded {
        name: String,
    },
    InputRemoved {
        name: String,
    },
    OutputAdded {
        name: String,
    },
    OutputRemoved {
        name: String,
    },
    SocketType {
        name: String,
        from: WireType,
        to: WireType,
    },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::ParamAdded { name } => write!(f, "+ parameter `{name}`"),
            Change::ParamRemoved { name } => write!(f, "- parameter `{name}`"),
            Change::ParamKind { name, from, to } => {
                write!(f, "~ parameter `{name}`: {from} → {to}")
            }
            Change::ParamDefault { name, from, to } => {
                write!(f, "~ parameter `{name}`: default {from} → {to}")
            }
            Change::InputAdded { name } => write!(f, "+ input `{name}`"),
            Change::InputRemoved { name } => write!(f, "- input `{name}`"),
            Change::OutputAdded { name } => write!(f, "+ output `{name}`"),
            Change::OutputRemoved { name } => write!(f, "- output `{name}`"),
            Change::SocketType { name, from, to } => {
                write!(f, "~ socket `{name}`: {from:?} → {to:?}")
            }
        }
    }
}

fn describe_kind(k: &ParamKind) -> String {
    let range = |a: Option<String>, b: Option<String>| match (a, b) {
        (None, None) => String::new(),
        (a, b) => format!(
            " {}…{}",
            a.unwrap_or_else(|| "−∞".into()),
            b.unwrap_or_else(|| "∞".into())
        ),
    };
    match k {
        ParamKind::Float { min, max } => format!(
            "float{}",
            range(min.map(|v| v.to_string()), max.map(|v| v.to_string()))
        ),
        ParamKind::Int { min, max } => format!(
            "int{}",
            range(min.map(|v| v.to_string()), max.map(|v| v.to_string()))
        ),
        ParamKind::Menu { options } => format!("menu [{}]", options.join(", ")),
        ParamKind::Vec { n } => format!("vec{n}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// The interface changes from `old` to `new`, in a stable order.
pub fn interface_changes(old: &OperatorSchema, new: &OperatorSchema) -> Vec<Change> {
    let mut out = Vec::new();
    for p in &old.params {
        match new.params.iter().find(|q| q.name == p.name) {
            None => out.push(Change::ParamRemoved {
                name: p.name.clone(),
            }),
            Some(q) => {
                if p.kind != q.kind {
                    out.push(Change::ParamKind {
                        name: p.name.clone(),
                        from: describe_kind(&p.kind),
                        to: describe_kind(&q.kind),
                    });
                }
                if p.default != q.default {
                    out.push(Change::ParamDefault {
                        name: p.name.clone(),
                        from: p.default.clone(),
                        to: q.default.clone(),
                    });
                }
            }
        }
    }
    for q in &new.params {
        if !old.params.iter().any(|p| p.name == q.name) {
            out.push(Change::ParamAdded {
                name: q.name.clone(),
            });
        }
    }
    let sockets = |a: &[SocketSchema], b: &[SocketSchema], out: &mut Vec<Change>, input: bool| {
        for s in a {
            match b.iter().find(|t| t.name == s.name) {
                None => out.push(if input {
                    Change::InputRemoved {
                        name: s.name.clone(),
                    }
                } else {
                    Change::OutputRemoved {
                        name: s.name.clone(),
                    }
                }),
                Some(t) if t.wire != s.wire => out.push(Change::SocketType {
                    name: s.name.clone(),
                    from: s.wire,
                    to: t.wire,
                }),
                Some(_) => {}
            }
        }
        for t in b {
            if !a.iter().any(|s| s.name == t.name) {
                out.push(if input {
                    Change::InputAdded {
                        name: t.name.clone(),
                    }
                } else {
                    Change::OutputAdded {
                        name: t.name.clone(),
                    }
                });
            }
        }
    };
    sockets(&old.inputs, &new.inputs, &mut out, true);
    sockets(&old.outputs, &new.outputs, &mut out, false);
    out
}

/// The result of [`PackSet::update`].
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateReport {
    pub pack: String,
    pub from: Version,
    pub to: Version,
    pub changes: Vec<Change>,
    /// `<network>/<instance>: what happened`, one line each.
    pub instances: Vec<String>,
}

impl UpdateReport {
    /// A major-version change (or a 0.x minor change): instances may need
    /// attention.
    pub fn breaking(&self) -> bool {
        !self.from.compatible_with(self.to)
    }
}

impl fmt::Display for UpdateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.to < self.from {
            "downgrade"
        } else if self.breaking() {
            "breaking"
        } else {
            "compatible"
        };
        writeln!(f, "{} {} → {} ({kind})", self.pack, self.from, self.to)?;
        for c in &self.changes {
            writeln!(f, "  {c}")?;
        }
        for i in &self.instances {
            writeln!(f, "  {i}")?;
        }
        Ok(())
    }
}

/// A network with every Pack instance expanded into the nodes inside it,
/// ready to cook ([`PackSet::flatten`]).
///
/// - Nodes outside any Pack keep their IDs and names.
/// - A node inside instance `glow1` is named `glow1__blur1`; its ID is
///   derived from the instance path and its ID inside the Pack (bit 62
///   set), so it is the same after every edit elsewhere and operator state
///   survives re-flattening.
/// - A promoted parameter's value (constant, binding or expression) is
///   copied onto every inner parameter it drives; inner bindings and wires
///   are remapped; `op('name')` in inner expressions is renamed to the flat
///   name. `me` in a promoted expression is the inner node it drives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Flat {
    pub network: Network,
    /// Inner node → its path for people, `glow1/blur1`.
    paths: BTreeMap<NodeId, String>,
    /// Inner node → the top-level instance it is in.
    instances: BTreeMap<NodeId, NodeId>,
    /// Top-level instance output → the flat endpoint that provides it.
    outputs: BTreeMap<(NodeId, String), Endpoint>,
}

impl Flat {
    /// The flat endpoint behind output `socket` of top-level instance
    /// `instance`.
    pub fn output(&self, instance: NodeId, socket: &str) -> Option<&Endpoint> {
        self.outputs.get(&(instance, socket.to_owned()))
    }

    /// The flat node that shows node `id` of the original network: itself,
    /// or for a Pack instance, the source of its first output.
    pub fn view(&self, id: NodeId) -> Option<NodeId> {
        if self.network.node(id).is_some() {
            return Some(id);
        }
        self.outputs
            .range((id, String::new())..)
            .take_while(|((i, _), _)| *i == id)
            .map(|(_, e)| e.node)
            .next()
    }

    /// The node's name for people: `glow1/blur1` inside a Pack.
    pub fn display(&self, id: NodeId) -> String {
        match self.paths.get(&id) {
            Some(p) => p.clone(),
            None => self
                .network
                .node(id)
                .map_or_else(|| id.to_string(), |n| n.name.clone()),
        }
    }

    /// The top-level Pack instance a flat node is inside.
    pub fn instance_of(&self, id: NodeId) -> Option<NodeId> {
        self.instances.get(&id).copied()
    }
}

/// The flat ID of node `local` inside the instance path `path` (FNV-1a).
fn inner_id(path: &[NodeId], local: NodeId) -> NodeId {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in path.iter().map(|n| n.0).chain([local.0]) {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    NodeId(INNER_ID_BIT | (h & (INNER_ID_BIT - 1)))
}

type Path = Vec<NodeId>;

/// A node of some network during flattening.
enum Local {
    Leaf(NodeId),
    Instance {
        inputs: BTreeMap<String, Vec<(Path, Endpoint)>>,
        outputs: BTreeMap<String, (Path, Endpoint)>,
        /// The first SIG output (what a binding to the instance follows).
        sig: Option<String>,
    },
}

struct ScopeInfo {
    names: BTreeMap<String, NodeId>,
}

struct Flattener<'a> {
    packs: &'a PackSet,
    registry: &'a Registry,
    locals: BTreeMap<(Path, NodeId), Local>,
    scopes: BTreeMap<Path, ScopeInfo>,
    nodes: Vec<Node>,
    /// Parameters with a binding or an expression, resolved at the end:
    /// (flat node, parameter, value, scope the value was written in).
    deferred: Vec<(NodeId, String, Param, Path)>,
    wires: Vec<((Path, Endpoint), (Path, Endpoint))>,
    used: BTreeSet<NodeId>,
    flat: Flat,
}

impl Flattener<'_> {
    fn walk(
        &mut self,
        net: &Network,
        path: &[NodeId],
        prefix: &str,
        display: &str,
        overrides: &BTreeMap<(NodeId, String), (Param, Path)>,
        depth: usize,
    ) -> Result<(), PackError> {
        if depth > MAX_DEPTH {
            return Err(PackError::TooDeep(display.trim_end_matches('/').to_owned()));
        }
        self.scopes.insert(
            path.to_vec(),
            ScopeInfo {
                names: net.nodes().map(|n| (n.name.clone(), n.id)).collect(),
            },
        );
        for node in net.nodes() {
            let mut params: BTreeMap<String, (Param, Path)> = node
                .params
                .iter()
                .map(|(k, p)| (k.clone(), (p.clone(), path.to_vec())))
                .collect();
            for ((_, pname), over) in overrides
                .range((node.id, String::new())..)
                .take_while(|((n, _), _)| *n == node.id)
            {
                params.insert(pname.clone(), over.clone());
            }
            if let Some(def) = self.packs.for_type(&node.type_id) {
                let mut child: Path = path.to_vec();
                child.push(node.id);
                let mut child_over = BTreeMap::new();
                for p in &def.params {
                    let v = params
                        .get(&p.name)
                        .cloned()
                        .unwrap_or_else(|| (Param::constant(p.default.clone()), path.to_vec()));
                    for t in &p.targets {
                        child_over.insert((t.node, t.param.clone()), v.clone());
                    }
                }
                let sig = self
                    .registry
                    .get(&node.type_id)
                    .and_then(|s| s.outputs.iter().find(|o| o.wire == WireType::Sig))
                    .map(|o| o.name.clone());
                self.locals.insert(
                    (path.to_vec(), node.id),
                    Local::Instance {
                        inputs: def
                            .inputs
                            .iter()
                            .map(|i| {
                                let t = i
                                    .targets
                                    .iter()
                                    .map(|e| (child.clone(), e.clone()))
                                    .collect();
                                (i.name.clone(), t)
                            })
                            .collect(),
                        outputs: def
                            .outputs
                            .iter()
                            .map(|o| (o.name.clone(), (child.clone(), o.source.clone())))
                            .collect(),
                        sig,
                    },
                );
                let prefix = format!("{prefix}{}{SCOPE_SEP}", node.name);
                let display = format!("{display}{}/", node.name);
                self.walk(
                    &def.network,
                    &child,
                    &prefix,
                    &display,
                    &child_over,
                    depth + 1,
                )?;
                continue;
            }
            let id = if path.is_empty() {
                if node.id.0 >= INNER_ID_BIT {
                    return Err(PackError::Network(format!(
                        "node {} has an ID too large to flatten",
                        node.name
                    )));
                }
                node.id
            } else {
                inner_id(path, node.id)
            };
            if !self.used.insert(id) {
                return Err(PackError::Network(format!(
                    "{display}{}: internal ID collision while flattening",
                    node.name
                )));
            }
            if let Some(&top) = path.first() {
                self.flat
                    .paths
                    .insert(id, format!("{display}{}", node.name));
                self.flat.instances.insert(id, top);
            }
            let default = |name: &str| {
                self.registry
                    .get(&node.type_id)
                    .and_then(|s| s.params.iter().find(|p| p.name == name))
                    .map(|p| &p.default)
            };
            let mut flat = Node {
                id,
                type_id: node.type_id.clone(),
                name: format!("{prefix}{}", node.name),
                pos: node.pos,
                params: BTreeMap::new(),
            };
            for (pname, (p, scope)) in params {
                if p.bind.is_some() || p.expr.is_some() {
                    self.deferred.push((id, pname, p, scope));
                } else if default(&pname) != Some(&p.value) {
                    flat.params.insert(pname, p);
                }
            }
            self.nodes.push(flat);
            self.locals
                .insert((path.to_vec(), node.id), Local::Leaf(id));
        }
        for w in net.wires() {
            self.wires
                .push(((path.to_vec(), w.from), (path.to_vec(), w.to)));
        }
        Ok(())
    }

    fn local(&self, path: &Path, id: NodeId) -> Result<&Local, PackError> {
        self.locals
            .get(&(path.clone(), id))
            .ok_or_else(|| PackError::Network(format!("reference to missing node {id}")))
    }

    /// The flat output behind an output endpoint of a local node.
    fn source(&self, mut at: (Path, Endpoint)) -> Result<Endpoint, PackError> {
        for _ in 0..=MAX_DEPTH + 1 {
            match self.local(&at.0, at.1.node)? {
                Local::Leaf(id) => return Ok(Endpoint::new(*id, at.1.socket)),
                Local::Instance { outputs, .. } => {
                    at = outputs.get(&at.1.socket).cloned().ok_or_else(|| {
                        PackError::Network(format!("Pack instance has no output `{}`", at.1.socket))
                    })?;
                }
            }
        }
        Err(PackError::TooDeep("output chain".into()))
    }

    /// The flat inputs behind an input endpoint of a local node.
    fn dests(
        &self,
        at: &(Path, Endpoint),
        out: &mut Vec<Endpoint>,
        depth: usize,
    ) -> Result<(), PackError> {
        if depth > MAX_DEPTH + 1 {
            return Err(PackError::TooDeep("input chain".into()));
        }
        match self.local(&at.0, at.1.node)? {
            Local::Leaf(id) => out.push(Endpoint::new(*id, at.1.socket.clone())),
            Local::Instance { inputs, .. } => {
                let targets = inputs.get(&at.1.socket).ok_or_else(|| {
                    PackError::Network(format!("Pack instance has no input `{}`", at.1.socket))
                })?;
                for t in targets {
                    self.dests(t, out, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// The flat node a binding to local node `id` follows.
    fn bound(&self, path: &Path, id: NodeId) -> Result<NodeId, PackError> {
        match self.local(path, id)? {
            Local::Leaf(id) => Ok(*id),
            Local::Instance { outputs, sig, .. } => {
                let s = sig.as_ref().and_then(|s| outputs.get(s)).ok_or_else(|| {
                    PackError::Network("a binding follows a Pack with no SIG output".into())
                })?;
                Ok(self.source(s.clone())?.node)
            }
        }
    }

    /// Renames `op('x')` references for scope `path`.
    fn rewrite(&self, src: &str, path: &Path, names: &BTreeMap<NodeId, String>) -> String {
        let Some(scope) = self.scopes.get(path) else {
            return src.to_owned();
        };
        rewrite_refs(src, |name| {
            let local = *scope.names.get(name)?;
            let flat = self.bound(path, local).ok()?;
            let new = names.get(&flat)?;
            (new != name).then(|| new.clone())
        })
        .unwrap_or_else(|| src.to_owned())
    }

    fn finish(mut self, top: &Network) -> Result<Flat, PackError> {
        let names: BTreeMap<NodeId, String> =
            self.nodes.iter().map(|n| (n.id, n.name.clone())).collect();
        let display = |flat: &Flat, id: NodeId| match flat.paths.get(&id) {
            Some(p) => p.clone(),
            None => names.get(&id).cloned().unwrap_or_else(|| id.to_string()),
        };
        let mut net = Network::default();
        let nodes = std::mem::take(&mut self.nodes);
        for node in nodes {
            let id = node.id;
            net.apply(self.registry, Edit::AddNode { node })
                .map_err(|e| PackError::Network(format!("{}: {e}", display(&self.flat, id))))?;
        }
        let mut dests = Vec::new();
        for (from, to) in &self.wires {
            let src = self.source(from.clone())?;
            dests.clear();
            self.dests(to, &mut dests, 0)?;
            for d in &dests {
                let wire = Wire::new(src.clone(), d.clone());
                net.apply(self.registry, Edit::Connect { wire })
                    .map_err(|e| {
                        PackError::Network(format!(
                            "{}.{} → {}.{}: {e}",
                            display(&self.flat, src.node),
                            src.socket,
                            display(&self.flat, d.node),
                            d.socket
                        ))
                    })?;
            }
        }
        for (id, pname, mut p, scope) in std::mem::take(&mut self.deferred) {
            if let Some(b) = &mut p.bind {
                b.node = self.bound(&scope, b.node)?;
            }
            if let Some(e) = &p.expr {
                p.expr = Some(self.rewrite(e, &scope, &names));
            }
            net.apply(
                self.registry,
                Edit::SetParam {
                    node: id,
                    param: pname.clone(),
                    value: Some(p),
                },
            )
            .map_err(|e| PackError::Network(format!("{}.{pname}: {e}", display(&self.flat, id))))?;
        }
        for node in top.nodes() {
            if let Some(Local::Instance { outputs, .. }) = self.locals.get(&(Vec::new(), node.id)) {
                for (socket, at) in outputs {
                    let src = self.source(at.clone())?;
                    self.flat.outputs.insert((node.id, socket.clone()), src);
                }
            }
        }
        self.flat.network = net;
        Ok(self.flat)
    }
}

/// Rewrites the node names in `op('name')` / `op("name")` references with
/// `rename` (`None` keeps a name). Returns `None` if nothing changed.
pub fn rewrite_refs(source: &str, rename: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(source.len());
    let mut changed = false;
    let mut pos = 0;
    while let Some(i) = source[pos..].find("op(") {
        let at = pos + i;
        let after = at + 3;
        let word_before = source[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let rest = &source[after..];
        let trimmed = rest.trim_start();
        let quote_at = after + (rest.len() - trimmed.len());
        let quote = trimmed.chars().next().filter(|c| *c == '\'' || *c == '"');
        let (Some(q), false) = (quote, word_before) else {
            out.push_str(&source[pos..after]);
            pos = after;
            continue;
        };
        let Some(end) = source[quote_at + 1..].find(q) else {
            break;
        };
        let name = &source[quote_at + 1..quote_at + 1 + end];
        out.push_str(&source[pos..quote_at + 1]);
        match rename(name) {
            Some(new) => {
                out.push_str(&new);
                changed = true;
            }
            None => out.push_str(name),
        }
        pos = quote_at + 1 + end;
    }
    out.push_str(&source[pos..]);
    changed.then_some(out)
}

/// The result of [`make_pack`].
#[derive(Debug, Clone, PartialEq)]
pub struct MadePack {
    /// The new, unlocked Pack (version 0.1.0).
    pub def: PackDef,
    /// Edits that replace the selection with one instance of it. Add the
    /// Pack to the set and register it before applying them.
    pub edits: Vec<Edit>,
    /// Things the user should know (expressions outside the selection that
    /// named a node now inside the Pack).
    pub warnings: Vec<String>,
}

/// Turns the selected nodes of `net` into a new Pack `name` and an instance
/// of it with ID `instance` (FR-P4 "Make Pack").
///
/// - Wires from outside become Pack inputs (one per outside source), wires
///   to outside become outputs (one per inside source); with none, the
///   last selected node nothing inside reads becomes output `out`.
/// - `promote` lists inner parameters to promote; their current values
///   become the defaults.
/// - A selected parameter bound to a node outside, or with an expression
///   naming a node outside, is promoted automatically and the instance
///   keeps the binding or expression.
/// - Outside parameters bound to a selected node follow the instance when
///   that node becomes its first SIG output.
pub fn make_pack(
    net: &Network,
    registry: &Registry,
    selection: &BTreeSet<NodeId>,
    name: &str,
    promote: &[ParamTarget],
    instance: NodeId,
) -> Result<MadePack, PackError> {
    if !valid_pack_name(name) {
        return Err(PackError::InvalidName(name.into()));
    }
    let err = |m: String| PackError::Invalid {
        pack: name.into(),
        message: m,
    };
    if selection.is_empty() {
        return Err(err("select at least one node".into()));
    }
    if net.node(instance).is_some() {
        return Err(err(format!("node {instance} already exists")));
    }
    let mut map = BTreeMap::new();
    for (i, id) in selection.iter().enumerate() {
        if net.node(*id).is_none() {
            return Err(err(format!("no node {id}")));
        }
        map.insert(*id, NodeId(i as u64));
    }
    let selected_names: BTreeSet<&str> = selection
        .iter()
        .filter_map(|id| net.node(*id))
        .map(|n| n.name.as_str())
        .collect();
    let origin = selection
        .iter()
        .filter_map(|id| net.node(*id))
        .fold([f32::INFINITY; 2], |o, n| {
            [o[0].min(n.pos[0]), o[1].min(n.pos[1])]
        });
    let schema_of = |id: NodeId| {
        net.node(id)
            .and_then(|n| registry.get(&n.type_id))
            .ok_or_else(|| err(format!("unknown operator on node {id}")))
    };

    // Inner nodes; parameters that reach outside are promoted.
    let mut inner = Network::default();
    let mut auto: Vec<(NodeId, String, Param)> = Vec::new();
    let mut inner_binds = Vec::new();
    for id in selection {
        let node = net.node(*id).expect("checked");
        let schema = schema_of(*id)?;
        let mut copy = node.clone();
        copy.id = map[id];
        copy.pos = [node.pos[0] - origin[0], node.pos[1] - origin[1]];
        copy.params.clear();
        for (pname, p) in &node.params {
            let outside_bind = p.bind.as_ref().is_some_and(|b| !map.contains_key(&b.node));
            let outside_expr = p.expr.as_ref().is_some_and(|e| {
                crate::expr::python_refs(e)
                    .iter()
                    .any(|r| !selected_names.contains(r.as_str()))
            });
            if outside_bind || outside_expr {
                auto.push((*id, pname.clone(), p.clone()));
                let default = schema
                    .params
                    .iter()
                    .find(|q| &q.name == pname)
                    .map(|q| &q.default);
                if default != Some(&p.value) {
                    copy.params
                        .insert(pname.clone(), Param::constant(p.value.clone()));
                }
            } else if let Some(b) = &p.bind {
                let mut q = p.clone();
                q.bind = Some(crate::graph::ChannelRef {
                    node: map[&b.node],
                    channel: b.channel.clone(),
                });
                inner_binds.push(Edit::SetParam {
                    node: map[id],
                    param: pname.clone(),
                    value: Some(q),
                });
            } else {
                copy.params.insert(pname.clone(), p.clone());
            }
        }
        inner
            .apply(registry, Edit::AddNode { node: copy })
            .map_err(|e| err(e.to_string()))?;
    }
    let mut inputs: Vec<PackInput> = Vec::new();
    let mut outward: BTreeMap<Endpoint, Vec<Endpoint>> = BTreeMap::new();
    let mut inward: BTreeMap<Endpoint, Vec<Endpoint>> = BTreeMap::new();
    for w in net.wires() {
        match (map.get(&w.from.node), map.get(&w.to.node)) {
            (Some(f), Some(t)) => {
                let wire = Wire::new(
                    Endpoint::new(*f, w.from.socket.clone()),
                    Endpoint::new(*t, w.to.socket.clone()),
                );
                inner
                    .apply(registry, Edit::Connect { wire })
                    .map_err(|e| err(e.to_string()))?;
            }
            (None, Some(t)) => inward
                .entry(w.from.clone())
                .or_default()
                .push(Endpoint::new(*t, w.to.socket.clone())),
            (Some(f), None) => outward
                .entry(Endpoint::new(*f, w.from.socket.clone()))
                .or_default()
                .push(w.to.clone()),
            (None, None) => {}
        }
    }
    for e in inner_binds {
        inner.apply(registry, e).map_err(|e| err(e.to_string()))?;
    }
    let unique = |base: &str, taken: &dyn Fn(&str) -> bool| {
        if !taken(base) {
            return base.to_owned();
        }
        (2..)
            .map(|n| format!("{base}_{n}"))
            .find(|n| !taken(n))
            .expect("a free name exists")
    };
    let mut outer_inputs = Vec::new();
    for (from, targets) in &inward {
        let name = unique(&targets[0].socket, &|n| inputs.iter().any(|i| i.name == n));
        outer_inputs.push((from.clone(), name.clone()));
        inputs.push(PackInput {
            name,
            targets: targets.clone(),
        });
    }

    // Outside bindings to selected nodes.
    let mut rebinds = Vec::new();
    for other in net.nodes().filter(|n| !map.contains_key(&n.id)) {
        for (pname, p) in &other.params {
            if let Some(b) = p.bind.as_ref().filter(|b| map.contains_key(&b.node)) {
                rebinds.push((other.id, pname.clone(), p.clone(), b.node));
            }
        }
    }
    let mut outputs: Vec<PackOutput> = Vec::new();
    let mut outer_outputs = Vec::new();
    let add_output = |source: Endpoint, outputs: &mut Vec<PackOutput>| -> String {
        if let Some(o) = outputs.iter().find(|o| o.source == source) {
            return o.name.clone();
        }
        let name = unique(&source.socket, &|n| outputs.iter().any(|o| o.name == n));
        outputs.push(PackOutput {
            name: name.clone(),
            source,
        });
        name
    };
    // SIG sources that outside bindings follow come first, so the first one
    // is the instance's first SIG output.
    for (_, _, _, src) in &rebinds {
        let schema = schema_of(*src)?;
        if let Some(o) = schema.outputs.iter().find(|o| o.wire == WireType::Sig) {
            add_output(Endpoint::new(map[src], o.name.clone()), &mut outputs);
        }
    }
    for (source, dests) in &outward {
        let name = add_output(source.clone(), &mut outputs);
        outer_outputs.push((name, dests.clone()));
    }
    if outputs.is_empty() {
        let read: BTreeSet<NodeId> = inner.wires().map(|w| w.from.node).collect();
        let last = selection
            .iter()
            .rev()
            .find(|id| {
                !read.contains(&map[id]) && schema_of(**id).is_ok_and(|s| !s.outputs.is_empty())
            })
            .or_else(|| {
                selection
                    .iter()
                    .rev()
                    .find(|id| schema_of(**id).is_ok_and(|s| !s.outputs.is_empty()))
            });
        if let Some(id) = last {
            let socket = schema_of(*id)?.outputs[0].name.clone();
            outputs.push(PackOutput {
                name: "out".into(),
                source: Endpoint::new(map[id], socket),
            });
        }
    }

    // Promoted parameters: chosen ones, then automatic ones.
    let mut params: Vec<PromotedParam> = Vec::new();
    let mut overrides = Vec::new();
    let promote_one =
        |id: NodeId, pname: &str, params: &mut Vec<PromotedParam>| -> Result<String, PackError> {
            let target = ParamTarget::new(map[&id], pname);
            if let Some(p) = params.iter().find(|p| p.targets.contains(&target)) {
                return Ok(p.name.clone());
            }
            let schema = schema_of(id)?;
            let ps = schema
                .params
                .iter()
                .find(|q| q.name == pname)
                .ok_or_else(|| err(format!("`{}` has no parameter `{pname}`", schema.type_id)))?;
            let node = net.node(id).expect("checked");
            let node_name = node.name.to_ascii_lowercase();
            let base = if params.iter().any(|p| p.name == pname) {
                format!("{node_name}_{pname}")
            } else {
                pname.to_owned()
            };
            let name = unique(&base, &|n| params.iter().any(|p| p.name == n));
            let value = node
                .params
                .get(pname)
                .map_or_else(|| ps.default.clone(), |p| p.value.clone());
            params.push(PromotedParam {
                name: name.clone(),
                label: String::new(),
                default: value,
                min: None,
                max: None,
                help: String::new(),
                targets: vec![target],
            });
            Ok(name)
        };
    for t in promote {
        if !map.contains_key(&t.node) {
            return Err(err(format!("node {} is not selected", t.node)));
        }
        promote_one(t.node, &t.param, &mut params)?;
    }
    for (id, pname, p) in &auto {
        let promoted = promote_one(*id, pname, &mut params)?;
        overrides.push((promoted, p.clone()));
    }

    let def = PackDef {
        name: name.into(),
        label: String::new(),
        params,
        inputs,
        outputs,
        network: inner,
        ..PackDef::new(name)
    };

    // The outer edits.
    let mut edits: Vec<Edit> = selection
        .iter()
        .map(|id| Edit::RemoveNode { id: *id })
        .collect();
    let mut node = Node::new(instance, def.type_id(), net.unique_name(name));
    node.pos = origin;
    edits.push(Edit::AddNode { node });
    for (from, input) in outer_inputs {
        edits.push(Edit::Connect {
            wire: Wire::new(from, Endpoint::new(instance, input)),
        });
    }
    for (output, dests) in outer_outputs {
        for d in dests {
            edits.push(Edit::Connect {
                wire: Wire::new(Endpoint::new(instance, output.clone()), d),
            });
        }
    }
    for (param, value) in overrides {
        edits.push(Edit::SetParam {
            node: instance,
            param,
            value: Some(value),
        });
    }
    let first_sig = def
        .outputs
        .iter()
        .find(|o| inner_output_wire(&def, registry, o) == Some(WireType::Sig));
    let mut warnings = Vec::new();
    for (node, pname, mut p, src) in rebinds {
        let follows = first_sig.is_some_and(|o| o.source.node == map[&src]);
        let who = net.node(node).map_or_else(String::new, |n| n.name.clone());
        if follows {
            if let Some(b) = &mut p.bind {
                b.node = instance;
            }
            edits.push(Edit::SetParam {
                node,
                param: pname,
                value: Some(p),
            });
        } else {
            warnings.push(format!(
                "{who}.{pname} no longer follows a channel: its source is now inside the Pack"
            ));
        }
    }
    for other in net.nodes().filter(|n| !map.contains_key(&n.id)) {
        for (pname, p) in &other.params {
            if let Some(e) = &p.expr
                && crate::expr::python_refs(e)
                    .iter()
                    .any(|r| selected_names.contains(r.as_str()))
            {
                warnings.push(format!(
                    "{}.{pname}: its expression names a node that is now inside the Pack",
                    other.name
                ));
            }
        }
    }
    Ok(MadePack {
        def,
        edits,
        warnings,
    })
}

fn inner_output_wire(def: &PackDef, registry: &Registry, o: &PackOutput) -> Option<WireType> {
    let node = def.network.node(o.source.node)?;
    registry
        .get(&node.type_id)?
        .outputs
        .iter()
        .find(|s| s.name == o.source.socket)
        .map(|s| s.wire)
}

#[cfg(test)]
mod tests;
