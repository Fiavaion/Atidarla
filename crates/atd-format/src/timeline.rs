//! The project's timeline as a TOML file (`timeline.toml`; M3 FR-T1–T3,
//! decision A54).
//!
//! ```toml
//! # Atidarla timeline. Times are in seconds; tracks and cues refer to node IDs.
//! format = 1
//! length = 32.0
//! fps = 60.0
//! loop = [8.0, 16.0]
//! stage = "follow"
//!
//! [clock]
//! node = 12
//! offset = -3600.0
//!
//! [[track]]
//! node = 3
//! param = "rotation"
//! keys = [
//!     { t = 0.0, v = 0.0 },
//!     { t = 4.0, v = 90.0, interp = "bezier", out = [1.0, 0.0] },
//!     { t = 8.0, v = 180.0, interp = "step", in = [-1.0, 0.0] },
//! ]
//!
//! [[track]]
//! node = 5
//! param = "color"
//! component = 2
//! keys = [
//!     { t = 0.0, v = 1.0 },
//! ]
//!
//! [[marker]]
//! id = 1
//! t = 8.0
//! name = "Drop"
//!
//! [[cue]]
//! id = 2
//! name = "Chorus"
//! t = 16.0
//! when = "op('beat1')['beat'] > 0"
//! actions = [
//!     { launch = 1, layers = [7, 8] },
//!     { node = 4, param = "opacity", live = 0.5 },
//!     { node = 4, param = "blend", set = "add" },
//! ]
//! ```
//!
//! - A track is identified by (node ID, parameter, component); `component`
//!   is written only for one channel of a vector or colour parameter.
//!   Tracks are sorted by that key, keys by time, markers by (time, ID),
//!   cues by (time, ID) with manual cues (no `t`) last, so the same
//!   timeline always gives the same bytes.
//! - Each key, and each cue action, is one line: moving a key or changing
//!   a value is a one-line diff.
//! - `interp` (default `linear`) shapes the segment from a key to the next:
//!   `step` holds the value, `linear` ramps, `bezier` follows a cubic curve
//!   through the key's `out` handle and the next key's `in` handle, each
//!   `[seconds, value]` relative to its key (default: a third of the
//!   segment, flat, which eases in and out).
//! - `stage` is `drive` (the default, not written: cues launch clips) or
//!   `follow` (launches on the Stage advance the cue list).
//! - `[clock]`, when present, chases the Timecode In node `node` (MTC or
//!   LTC); timeline time = timecode + `offset` seconds (default 0, not
//!   written). Without it the timeline runs on the internal clock.
//! - Reading rejects unknown keys, so a typo in a hand-edited file is an
//!   error rather than silently ignored.

use atd_core::graph::NodeId;
use serde_json::Value as Json;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::network::{
    check_format, invalid, json_to_toml, only_keys, required_str, required_u64, string, tables,
    toml_to_json,
};
use crate::{FORMAT, FormatError};

/// The file name inside a project folder.
pub const FILE: &str = "timeline.toml";

const HEADER: &str =
    "# Atidarla timeline. Times are in seconds; tracks and cues refer to node IDs.\n";

/// A project's timeline: keyframe tracks, markers and cues.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineDoc {
    /// Length in seconds (the ruler's end; playback stops or loops there).
    pub length: f64,
    /// Frame rate for display, snapping and timecode, frames per second.
    pub fps: f64,
    /// Loop region `[start, end]` in seconds, when looping is set up.
    pub loop_region: Option<[f64; 2]>,
    /// Follow the Stage (its launches advance the cue list) instead of
    /// driving it (cues launch).
    pub follow_stage: bool,
    /// The timecode chased; `None` runs on the internal clock.
    pub clock: Option<TimecodeClock>,
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub cues: Vec<Cue>,
}

impl Default for TimelineDoc {
    fn default() -> Self {
        Self {
            length: 60.0,
            fps: 60.0,
            loop_region: None,
            follow_stage: false,
            clock: None,
            tracks: Vec::new(),
            markers: Vec::new(),
            cues: Vec::new(),
        }
    }
}

/// Chasing timecode read by a Timecode In node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimecodeClock {
    /// The Timecode In node (MTC or LTC).
    pub node: NodeId,
    /// Seconds added to the timecode: timeline time = timecode + offset.
    pub offset: f64,
}

/// Keyframes animating one number: a parameter, or one component of a
/// vector or colour parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub node: NodeId,
    pub param: String,
    /// The component of a vector or colour parameter; `None` for a number.
    pub component: Option<usize>,
    /// Keys in time order, at distinct times.
    pub keys: Vec<Key>,
}

/// One keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    /// Time in seconds.
    pub t: f64,
    pub v: f64,
    /// The curve from this key to the next.
    pub interp: Interp,
    /// Bezier handle before the key, `[seconds, value]` relative to it.
    pub tan_in: Option<[f64; 2]>,
    /// Bezier handle after the key, `[seconds, value]` relative to it.
    pub tan_out: Option<[f64; 2]>,
}

impl Key {
    /// A key with linear interpolation and default handles.
    pub fn new(t: f64, v: f64) -> Self {
        Self {
            t,
            v,
            interp: Interp::Linear,
            tan_in: None,
            tan_out: None,
        }
    }

    pub fn with_interp(mut self, interp: Interp) -> Self {
        self.interp = interp;
        self
    }
}

/// How a segment between two keys is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interp {
    /// Hold the key's value until the next key.
    Step,
    #[default]
    Linear,
    /// A cubic Bezier through the keys' handles.
    Bezier,
}

impl Interp {
    pub const ALL: [Self; 3] = [Self::Step, Self::Linear, Self::Bezier];

    pub fn name(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Linear => "linear",
            Self::Bezier => "bezier",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.name() == name)
    }
}

/// A named point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    /// Stable ID, unique among markers and cues.
    pub id: u64,
    pub t: f64,
    pub name: String,
}

/// Something that happens at a time, or when triggered by hand.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    /// Stable ID, unique among markers and cues.
    pub id: u64,
    pub name: String,
    /// When the playhead fires it, in seconds; `None` for a manual cue,
    /// fired with Go (or by its condition).
    pub t: Option<f64>,
    /// A condition in the native expression subset (`atd_core::expr`),
    /// e.g. `op('beat1')['beat'] > 0`: a timed cue is armed at `t` and
    /// fires on the first frame the condition holds.
    pub when: Option<String>,
    pub actions: Vec<CueAction>,
}

/// What a cue does.
#[derive(Debug, Clone, PartialEq)]
pub enum CueAction {
    /// Change a parameter's stored value (an edit: undoable and saved).
    Set {
        node: NodeId,
        param: String,
        value: Json,
    },
    /// Hold a live value on a parameter until the transport stops (not an
    /// edit, not saved).
    Live {
        node: NodeId,
        param: String,
        value: Json,
    },
    /// Play `slot` on each layer: sets their `target` parameter (one column
    /// across a deck, or one clip on one layer).
    Launch { layers: Vec<NodeId>, slot: i64 },
}

impl TimelineDoc {
    /// Sorts everything into the canonical order (see the module docs).
    pub fn canonicalize(&mut self) {
        self.tracks
            .sort_by(|a, b| (a.node, &a.param, a.component).cmp(&(b.node, &b.param, b.component)));
        for t in &mut self.tracks {
            t.keys.sort_by(|a, b| a.t.total_cmp(&b.t));
        }
        self.markers
            .sort_by(|a, b| a.t.total_cmp(&b.t).then(a.id.cmp(&b.id)));
        self.cues.sort_by(|a, b| {
            let at = a.t.unwrap_or(f64::INFINITY);
            let bt = b.t.unwrap_or(f64::INFINITY);
            at.total_cmp(&bt).then(a.id.cmp(&b.id))
        });
    }

    /// An ID no marker or cue uses.
    pub fn next_id(&self) -> u64 {
        let m = self.markers.iter().map(|m| m.id);
        let c = self.cues.iter().map(|c| c.id);
        m.chain(c).max().map_or(1, |m| m + 1)
    }

    /// The track for (node, param, component), if any.
    pub fn track(&self, node: NodeId, param: &str, component: Option<usize>) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|t| t.node == node && t.param == param && t.component == component)
    }

    /// The track for (node, param, component), created empty (in canonical
    /// position) when missing.
    pub fn track_mut(&mut self, node: NodeId, param: &str, component: Option<usize>) -> &mut Track {
        let key = (node, param, component);
        let at = self
            .tracks
            .partition_point(|t| (t.node, t.param.as_str(), t.component) < key);
        let exists = self
            .tracks
            .get(at)
            .is_some_and(|t| (t.node, t.param.as_str(), t.component) == key);
        if !exists {
            self.tracks.insert(
                at,
                Track {
                    node,
                    param: param.to_owned(),
                    component,
                    keys: Vec::new(),
                },
            );
        }
        &mut self.tracks[at]
    }
}

impl Track {
    /// Adds a key at `t` with value `v`, or changes the value of the key
    /// already within half a millisecond of `t`. Returns its index.
    pub fn set_key(&mut self, t: f64, v: f64) -> usize {
        if let Some(i) = self.keys.iter().position(|k| (k.t - t).abs() < 5e-4) {
            self.keys[i].v = v;
            return i;
        }
        let at = self.keys.partition_point(|k| k.t < t);
        self.keys.insert(at, Key::new(t, v));
        at
    }
}

/// The timeline as TOML text. The same timeline always gives the same text.
pub fn timeline_to_string(doc: &TimelineDoc) -> Result<String, FormatError> {
    let mut doc = doc.clone();
    doc.canonicalize();
    let mut out = DocumentMut::new();
    out.insert("format", Item::Value(Value::from(FORMAT)));
    if let Some((mut key, _)) = out.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(HEADER);
    }
    out.insert("length", Item::Value(Value::from(doc.length)));
    out.insert("fps", Item::Value(Value::from(doc.fps)));
    if let Some([a, b]) = doc.loop_region {
        out.insert("loop", Item::Value(Value::Array(Array::from_iter([a, b]))));
    }
    if doc.follow_stage {
        out.insert("stage", Item::Value(Value::from("follow")));
    }
    if let Some(c) = doc.clock {
        let mut t = Table::new();
        t.insert("node", Item::Value(id(c.node.0)?));
        if c.offset != 0.0 {
            t.insert("offset", Item::Value(Value::from(c.offset)));
        }
        out.insert("clock", Item::Table(t));
    }

    let mut tracks = ArrayOfTables::new();
    for track in &doc.tracks {
        let mut t = Table::new();
        t.insert("node", Item::Value(id(track.node.0)?));
        t.insert("param", Item::Value(string(&track.param)));
        if let Some(c) = track.component {
            t.insert("component", Item::Value(Value::from(c as i64)));
        }
        let keys = track.keys.iter().map(|k| {
            let mut kt = InlineTable::new();
            kt.insert("t", Value::from(k.t));
            kt.insert("v", Value::from(k.v));
            if k.interp != Interp::Linear {
                kt.insert("interp", Value::from(k.interp.name()));
            }
            if let Some(h) = k.tan_in {
                kt.insert("in", Value::Array(Array::from_iter(h)));
            }
            if let Some(h) = k.tan_out {
                kt.insert("out", Value::Array(Array::from_iter(h)));
            }
            Value::InlineTable(kt)
        });
        t.insert("keys", Item::Value(Value::Array(one_per_line(keys))));
        tracks.push(t);
    }
    if !tracks.is_empty() {
        out.insert("track", Item::ArrayOfTables(tracks));
    }

    let mut markers = ArrayOfTables::new();
    for m in &doc.markers {
        let mut t = Table::new();
        t.insert("id", Item::Value(id(m.id)?));
        t.insert("t", Item::Value(Value::from(m.t)));
        t.insert("name", Item::Value(string(&m.name)));
        markers.push(t);
    }
    if !markers.is_empty() {
        out.insert("marker", Item::ArrayOfTables(markers));
    }

    let mut cues = ArrayOfTables::new();
    for c in &doc.cues {
        let mut t = Table::new();
        t.insert("id", Item::Value(id(c.id)?));
        t.insert("name", Item::Value(string(&c.name)));
        if let Some(time) = c.t {
            t.insert("t", Item::Value(Value::from(time)));
        }
        if let Some(when) = &c.when {
            t.insert("when", Item::Value(string(when)));
        }
        let mut actions = Vec::new();
        for a in &c.actions {
            let mut at = InlineTable::new();
            match a {
                CueAction::Set { node, param, value } | CueAction::Live { node, param, value } => {
                    at.insert("node", id(node.0)?);
                    at.insert("param", string(param));
                    let key = if matches!(a, CueAction::Set { .. }) {
                        "set"
                    } else {
                        "live"
                    };
                    let v = json_to_toml(value)
                        .map_err(|e| invalid(format!("cue {}, `{param}`: {e}", c.id)))?;
                    at.insert(key, v);
                }
                CueAction::Launch { layers, slot } => {
                    at.insert("launch", Value::from(*slot));
                    let mut ls = Array::new();
                    for l in layers {
                        ls.push(id(l.0)?);
                    }
                    at.insert("layers", Value::Array(ls));
                }
            }
            actions.push(Value::InlineTable(at));
        }
        t.insert(
            "actions",
            Item::Value(Value::Array(one_per_line(actions.into_iter()))),
        );
        cues.push(t);
    }
    if !cues.is_empty() {
        out.insert("cue", Item::ArrayOfTables(cues));
    }
    Ok(out.to_string())
}

/// Parses `timeline.toml`.
pub fn timeline_from_str(source: &str) -> Result<TimelineDoc, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(e.to_string().trim_end().to_owned()))?;
    let root = doc.as_table();
    only_keys(
        root,
        &[
            "format", "length", "fps", "loop", "stage", "clock", "track", "marker", "cue",
        ],
        "the file",
    )?;
    check_format(root)?;
    let mut out = TimelineDoc::default();
    if root.get("length").is_some() {
        out.length = number(root, "length", "the file")?;
        if out.length <= 0.0 {
            return Err(invalid("`length` must be more than 0 seconds".into()));
        }
    }
    if root.get("fps").is_some() {
        out.fps = number(root, "fps", "the file")?;
        if !(1.0..=1000.0).contains(&out.fps) {
            return Err(invalid("`fps` must be 1 to 1000".into()));
        }
    }
    if let Some(item) = root.get("loop") {
        let [a, b] = pair(item.as_value(), "`loop`")?;
        if !(0.0 <= a && a < b) {
            return Err(invalid(
                "`loop` must be [start, end] with 0 ≤ start < end".into(),
            ));
        }
        out.loop_region = Some([a, b]);
    }
    if root.get("stage").is_some() {
        out.follow_stage = match required_str(root, "stage", "the file")? {
            "drive" => false,
            "follow" => true,
            other => {
                return Err(invalid(format!(
                    "`stage` must be \"drive\" or \"follow\", not \"{other}\""
                )));
            }
        };
    }
    if let Some(item) = root.get("clock") {
        let c = item
            .as_table_like()
            .ok_or_else(|| invalid("`clock` must be a table".into()))?;
        only_keys(c, &["node", "offset"], "[clock]")?;
        out.clock = Some(TimecodeClock {
            node: NodeId(required_u64(c, "node", "[clock]")?),
            offset: match c.get("offset") {
                Some(_) => number(c, "offset", "[clock]")?,
                None => 0.0,
            },
        });
    }

    for (i, t) in tables(root.get("track"), "track")?.into_iter().enumerate() {
        let at = format!("track #{}", i + 1);
        only_keys(t, &["node", "param", "component", "keys"], &at)?;
        let node = NodeId(required_u64(t, "node", &at)?);
        let param = required_str(t, "param", &at)?.to_owned();
        let component = match t.get("component") {
            Some(_) => Some(required_u64(t, "component", &at)? as usize),
            None => None,
        };
        if out.track(node, &param, component).is_some() {
            return Err(invalid(format!(
                "{at}: a second track for node {}, `{param}`",
                node.0
            )));
        }
        let mut keys = Vec::new();
        if let Some(item) = t.get("keys") {
            let list = item
                .as_array()
                .ok_or_else(|| invalid(format!("{at}: `keys` must be a list of tables")))?;
            for (j, k) in list.iter().enumerate() {
                let at = format!("{at}, key #{}", j + 1);
                let k = k
                    .as_inline_table()
                    .ok_or_else(|| invalid(format!("{at}: expected {{ t = …, v = … }}")))?;
                keys.push(key(k, &at)?);
            }
        }
        keys.sort_by(|a, b| a.t.total_cmp(&b.t));
        if let Some(w) = keys.windows(2).find(|w| w[0].t == w[1].t) {
            return Err(invalid(format!("{at}: two keys at {} s", w[0].t)));
        }
        out.tracks.push(Track {
            node,
            param,
            component,
            keys,
        });
    }

    let mut ids = std::collections::BTreeSet::new();
    let mut unique = |id: u64, at: &str| {
        if ids.insert(id) {
            Ok(())
        } else {
            Err(invalid(format!(
                "{at}: ID {id} is used twice (marker and cue IDs are shared)"
            )))
        }
    };
    for (i, m) in tables(root.get("marker"), "marker")?
        .into_iter()
        .enumerate()
    {
        let at = format!("marker #{}", i + 1);
        only_keys(m, &["id", "t", "name"], &at)?;
        let id = required_u64(m, "id", &at)?;
        unique(id, &at)?;
        out.markers.push(Marker {
            id,
            t: time(m, "t", &at)?,
            name: match m.get("name") {
                Some(_) => required_str(m, "name", &at)?.to_owned(),
                None => String::new(),
            },
        });
    }
    for (i, c) in tables(root.get("cue"), "cue")?.into_iter().enumerate() {
        let at = format!("cue #{}", i + 1);
        only_keys(c, &["id", "name", "t", "when", "actions"], &at)?;
        let id = required_u64(c, "id", &at)?;
        unique(id, &at)?;
        let mut actions = Vec::new();
        if let Some(item) = c.get("actions") {
            let list = item
                .as_array()
                .ok_or_else(|| invalid(format!("{at}: `actions` must be a list of tables")))?;
            for (j, a) in list.iter().enumerate() {
                let at = format!("{at}, action #{}", j + 1);
                let a = a
                    .as_inline_table()
                    .ok_or_else(|| invalid(format!("{at}: expected a table")))?;
                actions.push(action(a, &at)?);
            }
        }
        out.cues.push(Cue {
            id,
            name: match c.get("name") {
                Some(_) => required_str(c, "name", &at)?.to_owned(),
                None => String::new(),
            },
            t: match c.get("t") {
                Some(_) => Some(time(c, "t", &at)?),
                None => None,
            },
            when: match c.get("when") {
                Some(_) => Some(required_str(c, "when", &at)?.to_owned()),
                None => None,
            },
            actions,
        });
    }
    out.canonicalize();
    Ok(out)
}

fn key(k: &InlineTable, at: &str) -> Result<Key, FormatError> {
    only_keys(k, &["t", "v", "interp", "in", "out"], at)?;
    let interp = match k.get("interp") {
        None => Interp::Linear,
        Some(v) => v.as_str().and_then(Interp::from_name).ok_or_else(|| {
            invalid(format!(
                "{at}: `interp` must be \"step\", \"linear\" or \"bezier\""
            ))
        })?,
    };
    let handle = |name: &str| match k.get(name) {
        None => Ok(None),
        Some(v) => pair(Some(v), &format!("{at}: `{name}`")).map(Some),
    };
    let v = k
        .get("v")
        .and_then(as_f64)
        .ok_or_else(|| invalid(format!("{at}: `v` must be a number")))?;
    Ok(Key {
        t: time(k, "t", at)?,
        v,
        interp,
        tan_in: handle("in")?,
        tan_out: handle("out")?,
    })
}

fn action(a: &InlineTable, at: &str) -> Result<CueAction, FormatError> {
    if a.contains_key("launch") {
        only_keys(a, &["launch", "layers"], at)?;
        let slot = a
            .get("launch")
            .and_then(Value::as_integer)
            .ok_or_else(|| invalid(format!("{at}: `launch` must be a slot number")))?;
        let layers = a
            .get("layers")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("{at}: `layers` must be a list of node IDs")))?
            .iter()
            .map(|v| {
                v.as_integer()
                    .and_then(|i| u64::try_from(i).ok())
                    .map(NodeId)
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid(format!("{at}: `layers` must be a list of node IDs")))?;
        return Ok(CueAction::Launch { layers, slot });
    }
    only_keys(a, &["node", "param", "set", "live"], at)?;
    let node = NodeId(required_u64(a, "node", at)?);
    let param = required_str(a, "param", at)?.to_owned();
    let value = |v: &Value| toml_to_json(v).map_err(|e| invalid(format!("{at}: {e}")));
    match (a.get("set"), a.get("live")) {
        (Some(v), None) => Ok(CueAction::Set {
            node,
            param,
            value: value(v)?,
        }),
        (None, Some(v)) => Ok(CueAction::Live {
            node,
            param,
            value: value(v)?,
        }),
        _ => Err(invalid(format!(
            "{at}: an action has `launch`, or `node`, `param` and one of `set` or `live`"
        ))),
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_float()
        .or_else(|| v.as_integer().map(|i| i as f64))
        .filter(|f| f.is_finite())
}

fn number(t: &dyn TableLike, key: &str, at: &str) -> Result<f64, FormatError> {
    t.get(key)
        .and_then(Item::as_value)
        .and_then(as_f64)
        .ok_or_else(|| invalid(format!("{at}: `{key}` must be a number")))
}

/// A time: a finite number of seconds, at least 0.
fn time(t: &dyn TableLike, key: &str, at: &str) -> Result<f64, FormatError> {
    let v = number(t, key, at)?;
    if v < 0.0 {
        return Err(invalid(format!("{at}: `{key}` must be 0 or more seconds")));
    }
    Ok(v)
}

fn pair(v: Option<&Value>, at: &str) -> Result<[f64; 2], FormatError> {
    let bad = || invalid(format!("{at}: expected two numbers, like [1.0, 0.0]"));
    let a = v.and_then(Value::as_array).ok_or_else(bad)?;
    let nums: Vec<f64> = a
        .iter()
        .map(as_f64)
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    match nums[..] {
        [x, y] => Ok([x, y]),
        _ => Err(bad()),
    }
}

fn id(v: u64) -> Result<Value, FormatError> {
    i64::try_from(v)
        .map(Value::from)
        .map_err(|_| invalid(format!("{v} is too large")))
}

/// A list written one item per line, so each item diffs on its own.
fn one_per_line(items: impl Iterator<Item = Value>) -> Array {
    let mut a = Array::new();
    for mut v in items {
        v.decor_mut().set_prefix("\n    ");
        v.decor_mut().set_suffix("");
        a.push_formatted(v);
    }
    if !a.is_empty() {
        a.set_trailing("\n");
        a.set_trailing_comma(true);
    }
    a
}
