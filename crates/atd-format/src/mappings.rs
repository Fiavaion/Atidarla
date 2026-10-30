//! MIDI and OSC mappings as a TOML file (`mappings.toml`, FR-S5, A54).
//!
//! ```toml
//! format = 1
//! next_id = 2
//!
//! [osc]
//! port = 9000
//! oscquery = true
//! oscquery_port = 9001
//!
//! [[mapping]]
//! id = 0
//! source = { midi = "cc", number = 7, channel = 1, device = "nanoKONTROL" }
//! target = { node = 3, param = "colour", component = 0 }
//! input = [0.0, 127.0]
//! output = [0.0, 1.0]
//! curve = "linear"
//! mode = "absolute"
//! pickup = false
//! ```
//!
//! - Mappings have stable IDs and are written sorted by ID; targets refer
//!   to node IDs in `networks/main.toml`, so renaming a node changes
//!   nothing here.
//! - `source` is a MIDI control (`midi = "cc" | "note" | "bend"`, with
//!   `number` for CC and notes, an optional `channel` 1–16 and an optional
//!   case-insensitive `device` name pattern; absent means any) or an OSC
//!   address (`osc = "/fader/1"`, `arg` = argument index).
//! - `curve` is `"linear"`, `{ exp = <amount> }` or `{ step = <levels> }`;
//!   `mode` is `"absolute"`, `"toggle"`, `"momentary"` or `"relative"`.
//! - Reading fills in what a hand-written file leaves out (the input range
//!   from the source, output 0–1, linear, absolute, no pick-up) and rejects
//!   unknown keys. Writing always gives every field, in one order.
//! - A project without the file has no mappings and OSC off.

use atd_core::graph::NodeId;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::network::{
    check_format, invalid, only_keys, required_str, required_u64, string, table_like, tables,
};
use crate::{FORMAT, FormatError};

/// File name inside the project folder.
pub const FILE: &str = "mappings.toml";

const HEADER: &str = "# Atidarla mappings: MIDI and OSC controls to parameters (docs/mappings.md).\n\
                      # Targets refer to node IDs in networks/main.toml.\n";

/// A project's mappings and OSC settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mappings {
    /// Sorted by ID.
    pub mappings: Vec<Mapping>,
    /// The ID the next mapping gets; IDs are never reused.
    pub next_id: u64,
    pub osc: OscSettings,
}

/// Where OSC is received, and whether the parameters are advertised with
/// OSCQuery.
#[derive(Debug, Clone, PartialEq)]
pub struct OscSettings {
    /// UDP port for mapped OSC and OSCQuery addresses.
    pub port: u16,
    /// Advertise the parameters (HTTP JSON and mDNS); off by default.
    pub oscquery: bool,
    /// TCP port of the OSCQuery HTTP server.
    pub oscquery_port: u16,
}

impl Default for OscSettings {
    fn default() -> Self {
        Self {
            port: 9000,
            oscquery: false,
            oscquery_port: 9001,
        }
    }
}

/// One control driving one parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    pub id: u64,
    pub source: Source,
    pub target: Target,
    /// Incoming values mapped to `output`; values outside are clamped.
    pub input: [f64; 2],
    /// Parameter values `input` maps to (may be reversed).
    pub output: [f64; 2],
    pub curve: Curve,
    pub mode: Mode,
    /// Soft takeover: the control takes the parameter over only once it
    /// passes the parameter's current value.
    pub pickup: bool,
}

/// What a MIDI source listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiControl {
    /// Control change `0..=127`.
    Cc(u8),
    /// Note `0..=127`: velocity as value, note-off as 0.
    Note(u8),
    /// Pitch bend, −8192…8191.
    Bend,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    Midi {
        control: MidiControl,
        /// 1–16; `None` = any channel.
        channel: Option<u8>,
        /// Case-insensitive part of the device name; empty = any device.
        device: String,
    },
    Osc {
        address: String,
        /// Which argument of the message (0 = first).
        arg: usize,
    },
}

impl Source {
    /// The range a source sends, as the default input range.
    pub fn natural_range(&self) -> [f64; 2] {
        match self {
            Self::Midi {
                control: MidiControl::Bend,
                ..
            } => [-8192.0, 8191.0],
            Self::Midi { .. } => [0.0, 127.0],
            Self::Osc { .. } => [0.0, 1.0],
        }
    }

    /// Short text for lists: `CC 7 ch1 (nano)`, `/fader/1 [0]`.
    pub fn label(&self) -> String {
        match self {
            Self::Midi {
                control,
                channel,
                device,
            } => {
                let mut s = match control {
                    MidiControl::Cc(n) => format!("CC {n}"),
                    MidiControl::Note(n) => format!("Note {n}"),
                    MidiControl::Bend => "Pitch bend".to_owned(),
                };
                match channel {
                    Some(c) => s.push_str(&format!(" ch{c}")),
                    None => s.push_str(" any ch"),
                }
                if !device.is_empty() {
                    s.push_str(&format!(" ({device})"));
                }
                s
            }
            Self::Osc { address, arg } => format!("{address} [{arg}]"),
        }
    }
}

/// A parameter: node by stable ID, parameter name, and for vector and
/// colour parameters the component (0 = x / red).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub node: NodeId,
    pub param: String,
    pub component: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Linear,
    /// `(e^(a·t) − 1) / (e^a − 1)`: positive amounts start slow, negative
    /// fast; 0 is linear.
    Exp(f64),
    /// Quantised to this many evenly spaced levels (≥ 2).
    Step(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The control's position sets the value.
    Absolute,
    /// Each press (rising past the middle of the input range) flips between
    /// the two ends of the output range.
    Toggle,
    /// The high end of the output range while held, the low end otherwise.
    Momentary,
    /// An endless encoder: values are steps (MIDI two's complement: 1–63
    /// up, 65–127 down; OSC the signed step), added to the current value.
    Relative,
}

impl Mode {
    pub const ALL: [Mode; 4] = [
        Self::Absolute,
        Self::Toggle,
        Self::Momentary,
        Self::Relative,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::Toggle => "toggle",
            Self::Momentary => "momentary",
            Self::Relative => "relative",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }
}

impl Mappings {
    /// Adds a mapping with defaults for the source (input range) and the
    /// given output range; returns its ID.
    pub fn add(&mut self, source: Source, target: Target, output: [f64; 2]) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let input = source.natural_range();
        self.mappings.push(Mapping {
            id,
            source,
            target,
            input,
            output,
            curve: Curve::Linear,
            mode: Mode::Absolute,
            pickup: false,
        });
        id
    }

    /// Removes the mapping `id`; whether it existed.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.mappings.len();
        self.mappings.retain(|m| m.id != id);
        self.mappings.len() != before
    }

    /// Whether there is nothing to write (no mappings, default OSC).
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty() && self.osc == OscSettings::default()
    }
}

fn float(v: f64) -> Value {
    Value::from(v)
}

fn range(r: [f64; 2]) -> Value {
    let mut a = Array::new();
    a.push(r[0]);
    a.push(r[1]);
    Value::Array(a)
}

fn int(v: u64) -> Result<Value, FormatError> {
    i64::try_from(v)
        .map(Value::from)
        .map_err(|_| invalid(format!("{v} is too large")))
}

/// `mappings.toml` as text. The same mappings always give the same text.
pub fn mappings_to_string(m: &Mappings) -> Result<String, FormatError> {
    let mut doc = DocumentMut::new();
    doc.insert("format", Item::Value(Value::from(FORMAT)));
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(HEADER);
    }
    let next = m
        .mappings
        .iter()
        .map(|x| x.id + 1)
        .max()
        .unwrap_or(0)
        .max(m.next_id);
    doc.insert("next_id", Item::Value(int(next)?));

    if m.osc != OscSettings::default() {
        let mut t = Table::new();
        t.insert("port", Item::Value(Value::from(i64::from(m.osc.port))));
        t.insert("oscquery", Item::Value(Value::from(m.osc.oscquery)));
        t.insert(
            "oscquery_port",
            Item::Value(Value::from(i64::from(m.osc.oscquery_port))),
        );
        doc.insert("osc", Item::Table(t));
    }

    let mut sorted: Vec<&Mapping> = m.mappings.iter().collect();
    sorted.sort_by_key(|x| x.id);
    let mut list = ArrayOfTables::new();
    for x in sorted {
        let mut t = Table::new();
        t.insert("id", Item::Value(int(x.id)?));
        let mut src = InlineTable::new();
        match &x.source {
            Source::Midi {
                control,
                channel,
                device,
            } => {
                let (kind, number) = match control {
                    MidiControl::Cc(n) => ("cc", Some(*n)),
                    MidiControl::Note(n) => ("note", Some(*n)),
                    MidiControl::Bend => ("bend", None),
                };
                src.insert("midi", Value::from(kind));
                if let Some(n) = number {
                    src.insert("number", Value::from(i64::from(n)));
                }
                if let Some(c) = channel {
                    src.insert("channel", Value::from(i64::from(*c)));
                }
                if !device.is_empty() {
                    src.insert("device", string(device));
                }
            }
            Source::Osc { address, arg } => {
                src.insert("osc", string(address));
                src.insert("arg", int(*arg as u64)?);
            }
        }
        t.insert("source", Item::Value(Value::InlineTable(src)));
        let mut tgt = InlineTable::new();
        tgt.insert("node", int(x.target.node.0)?);
        tgt.insert("param", string(&x.target.param));
        if let Some(c) = x.target.component {
            tgt.insert("component", int(c as u64)?);
        }
        t.insert("target", Item::Value(Value::InlineTable(tgt)));
        t.insert("input", Item::Value(range(x.input)));
        t.insert("output", Item::Value(range(x.output)));
        let curve = match x.curve {
            Curve::Linear => Value::from("linear"),
            Curve::Exp(a) => {
                let mut c = InlineTable::new();
                c.insert("exp", float(a));
                Value::InlineTable(c)
            }
            Curve::Step(n) => {
                let mut c = InlineTable::new();
                c.insert("step", Value::from(i64::from(n)));
                Value::InlineTable(c)
            }
        };
        t.insert("curve", Item::Value(curve));
        t.insert("mode", Item::Value(Value::from(x.mode.name())));
        t.insert("pickup", Item::Value(Value::from(x.pickup)));
        list.push(t);
    }
    if !list.is_empty() {
        doc.insert("mapping", Item::ArrayOfTables(list));
    }
    Ok(doc.to_string())
}

fn number(item: &Item) -> Option<f64> {
    item.as_float()
        .or_else(|| item.as_integer().map(|i| i as f64))
        .filter(|f| f.is_finite())
}

fn read_range(t: &dyn TableLike, key: &str, at: &str) -> Result<Option<[f64; 2]>, FormatError> {
    let Some(item) = t.get(key) else {
        return Ok(None);
    };
    let bad = || {
        invalid(format!(
            "{at}: `{key}` must be two numbers, like [0.0, 1.0]"
        ))
    };
    let a = item.as_array().ok_or_else(bad)?;
    let v: Vec<f64> = a
        .iter()
        .map(|v| {
            v.as_float()
                .or_else(|| v.as_integer().map(|i| i as f64))
                .filter(|f| f.is_finite())
        })
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    match v[..] {
        [a, b] => Ok(Some([a, b])),
        _ => Err(bad()),
    }
}

fn small(t: &dyn TableLike, key: &str, at: &str, max: u64) -> Result<Option<u64>, FormatError> {
    match t.get(key) {
        None => Ok(None),
        Some(_) => {
            let v = required_u64(t, key, at)?;
            if v > max {
                return Err(invalid(format!("{at}: `{key}` must be at most {max}")));
            }
            Ok(Some(v))
        }
    }
}

fn read_source(item: &Item, at: &str) -> Result<Source, FormatError> {
    let t = table_like(item, at)?;
    if t.contains_key("osc") {
        only_keys(t, &["osc", "arg"], at)?;
        let address = required_str(t, "osc", at)?;
        if !address.starts_with('/') {
            return Err(invalid(format!("{at}: an OSC address starts with `/`")));
        }
        let arg = small(t, "arg", at, 255)?.unwrap_or(0) as usize;
        return Ok(Source::Osc {
            address: address.to_owned(),
            arg,
        });
    }
    only_keys(t, &["midi", "number", "channel", "device"], at)?;
    let kind = t.get("midi").and_then(Item::as_str).ok_or_else(|| {
        invalid(format!(
            "{at}: needs `midi = \"cc\"` (or note, bend) or `osc`"
        ))
    })?;
    let number = || {
        small(t, "number", at, 127)?
            .map(|n| n as u8)
            .ok_or_else(|| invalid(format!("{at}: `number` (0–127) is required")))
    };
    let control = match kind {
        "cc" => MidiControl::Cc(number()?),
        "note" => MidiControl::Note(number()?),
        "bend" => {
            if t.contains_key("number") {
                return Err(invalid(format!("{at}: pitch bend has no `number`")));
            }
            MidiControl::Bend
        }
        other => {
            return Err(invalid(format!(
                "{at}: unknown MIDI control `{other}` (cc, note, bend)"
            )));
        }
    };
    let channel = small(t, "channel", at, 16)?.map(|c| c as u8);
    if channel == Some(0) {
        return Err(invalid(format!(
            "{at}: `channel` is 1–16 (leave it out for any)"
        )));
    }
    let device = match t.get("device") {
        None => String::new(),
        Some(_) => required_str(t, "device", at)?.to_owned(),
    };
    Ok(Source::Midi {
        control,
        channel,
        device,
    })
}

fn read_curve(item: Option<&Item>, at: &str) -> Result<Curve, FormatError> {
    let Some(item) = item else {
        return Ok(Curve::Linear);
    };
    let bad = || {
        invalid(format!(
            "{at}: `curve` is \"linear\", {{ exp = <amount> }} or {{ step = <levels ≥ 2> }}"
        ))
    };
    if let Some(s) = item.as_str() {
        return (s == "linear").then_some(Curve::Linear).ok_or_else(bad);
    }
    let t = item.as_table_like().ok_or_else(bad)?;
    if t.len() != 1 {
        return Err(bad());
    }
    if let Some(a) = t.get("exp") {
        return number(a).map(Curve::Exp).ok_or_else(bad);
    }
    if let Some(n) = t.get("step") {
        return n
            .as_integer()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n >= 2)
            .map(Curve::Step)
            .ok_or_else(bad);
    }
    Err(bad())
}

fn port(t: &dyn TableLike, key: &str, default: u16) -> Result<u16, FormatError> {
    match t.get(key) {
        None => Ok(default),
        Some(item) => item
            .as_integer()
            .and_then(|v| u16::try_from(v).ok())
            .ok_or_else(|| invalid(format!("[osc]: `{key}` must be a port number 0–65535"))),
    }
}

/// Parses `mappings.toml`. Targets are not checked against a network.
pub fn mappings_from_str(source: &str) -> Result<Mappings, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(e.to_string().trim_end().to_owned()))?;
    let root = doc.as_table();
    only_keys(root, &["format", "next_id", "osc", "mapping"], "the file")?;
    check_format(root)?;
    let mut osc = OscSettings::default();
    if let Some(item) = root.get("osc") {
        let t = table_like(item, "[osc]")?;
        only_keys(t, &["port", "oscquery", "oscquery_port"], "[osc]")?;
        osc.port = port(t, "port", osc.port)?;
        osc.oscquery_port = port(t, "oscquery_port", osc.oscquery_port)?;
        if let Some(v) = t.get("oscquery") {
            osc.oscquery = v
                .as_bool()
                .ok_or_else(|| invalid("[osc]: `oscquery` must be true or false".into()))?;
        }
    }
    let mut mappings: Vec<Mapping> = Vec::new();
    for (i, t) in tables(root.get("mapping"), "mapping")?
        .into_iter()
        .enumerate()
    {
        let at = format!("mapping {}", i + 1);
        only_keys(
            t,
            &[
                "id", "source", "target", "input", "output", "curve", "mode", "pickup",
            ],
            &at,
        )?;
        let id = required_u64(t, "id", &at)?;
        let at = format!("mapping {id}");
        if mappings.iter().any(|m| m.id == id) {
            return Err(invalid(format!("{at}: the ID is used twice")));
        }
        let source = read_source(
            t.get("source")
                .ok_or_else(|| invalid(format!("{at}: missing `source`")))?,
            &format!("{at}, source"),
        )?;
        let tt = table_like(
            t.get("target")
                .ok_or_else(|| invalid(format!("{at}: missing `target`")))?,
            &format!("{at}, target"),
        )?;
        let tat = format!("{at}, target");
        only_keys(tt, &["node", "param", "component"], &tat)?;
        let param = required_str(tt, "param", &tat)?;
        if param.is_empty() {
            return Err(invalid(format!("{tat}: `param` is empty")));
        }
        let target = Target {
            node: NodeId(required_u64(tt, "node", &tat)?),
            param: param.to_owned(),
            component: small(tt, "component", &tat, 15)?.map(|c| c as usize),
        };
        let mode = match t.get("mode") {
            None => Mode::Absolute,
            Some(item) => item.as_str().and_then(Mode::parse).ok_or_else(|| {
                invalid(format!(
                    "{at}: `mode` is absolute, toggle, momentary or relative"
                ))
            })?,
        };
        let pickup = match t.get("pickup") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| invalid(format!("{at}: `pickup` must be true or false")))?,
        };
        mappings.push(Mapping {
            id,
            input: read_range(t, "input", &at)?.unwrap_or_else(|| source.natural_range()),
            output: read_range(t, "output", &at)?.unwrap_or([0.0, 1.0]),
            curve: read_curve(t.get("curve"), &at)?,
            source,
            target,
            mode,
            pickup,
        });
    }
    mappings.sort_by_key(|m| m.id);
    let max = mappings.last().map_or(0, |m| m.id + 1);
    let next_id = match root.get("next_id") {
        None => max,
        Some(_) => required_u64(root, "next_id", "the file")?.max(max),
    };
    Ok(Mappings {
        mappings,
        next_id,
        osc,
    })
}
