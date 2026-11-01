//! Batch wedges (FR-B1, FR-B2) as TOML files: `batch/<name>.toml`.
//!
//! ```toml
//! format = 1
//! target = 3                    # the IMG node whose image each item keeps (ID in networks/main.toml)
//! frame = 0                     # frame each item cooks (at frame / fps seconds)
//! fps = 60.0
//! size = [512, 512]             # optional: the project resolution when absent
//! combine = "product"           # or "zip"
//! scheduler = "lan:*"           # "local", "lan:*", a host, or a list of hosts
//! concurrency = 1               # items in flight per host
//!
//! [[axis]]
//! node = 3
//! param = "prompt1"
//! name = "prompt"               # optional attribute name; default <node name>.<param>
//! values = ["a red fox", "a blue whale"]
//!
//! [[axis]]
//! node = 3
//! param = "seed"
//! seeds = { from = 1, count = 10 }
//!
//! [[axis]]
//! node = 5
//! param = "strength"
//! range = { from = 0.2, to = 0.8, steps = 4 }
//! ```
//!
//! - A wedge is a list of axes, each over one parameter of a node of the
//!   main network: a list of values, a numeric range in steps (both ends
//!   included) or a seed range. `product` makes one work item per
//!   combination (the last axis varies fastest), `zip` one per position
//!   (every axis the same length).
//! - Hosts are AI Host names (`local`, `lan:<x>`, `cloud:<x>`) or ComfyUI
//!   servers (`comfy:<host:port>`); `lan:*` is every LAN host that can run
//!   the target's AI nodes.
//! - Nodes are referred to by ID, so renaming a node changes nothing here.
//!   Reading fills in what a hand-written file leaves out and rejects
//!   unknown keys; writing gives every field in one order (`size` and an
//!   axis `name` only when set), so saving the same wedge gives the same
//!   bytes (CLAUDE.md rule 8).

use std::path::{Path, PathBuf};

use lsr_core::graph::NodeId;
use serde_json::Value as Json;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::models::{valid_host_addr, valid_host_name};
use crate::network::{
    check_format, invalid, json_to_toml, only_keys, required_str, required_u64, string, table_like,
    tables, toml_to_json,
};
use crate::{FORMAT, FormatError};

/// Folder of wedge files inside the project folder.
pub const DIR: &str = "batch";

/// Most work items one wedge may make.
pub const MAX_ITEMS: usize = 100_000;

/// Most items in flight per host.
pub const MAX_CONCURRENCY: u32 = 64;

const HEADER: &str = "# Lasair wedge: a batch of work items over parameters (docs/batch.md).\n\
                      # Nodes are IDs in networks/main.toml.\n";

/// How axes combine into work items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Combine {
    /// Every combination; the last axis varies fastest.
    #[default]
    Product,
    /// Item `i` takes value `i` of every axis.
    Zip,
}

impl Combine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Zip => "zip",
        }
    }
}

/// Where work items run (FR-B2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheduler {
    /// `lan:*`: every LAN host that can run the target's AI nodes, taking
    /// items in turn.
    Lan,
    /// These hosts, in this order: `local`, `lan:<x>`, `cloud:<x>` or
    /// `comfy:<host:port>`.
    Hosts(Vec<String>),
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::Hosts(vec!["local".into()])
    }
}

/// A ComfyUI server address from a `comfy:<host:port>` host.
pub fn comfy_addr(host: &str) -> Option<&str> {
    host.strip_prefix("comfy:")
}

/// A host a scheduler may name.
pub fn valid_batch_host(host: &str) -> bool {
    host == "local" || valid_host_name(host) || comfy_addr(host).is_some_and(valid_host_addr)
}

/// The values an axis takes.
#[derive(Debug, Clone, PartialEq)]
pub enum AxisValues {
    /// Any parameter values (strings, numbers, colours, …).
    List(Vec<Json>),
    /// `steps` numbers from `from` to `to`, both included.
    Range { from: f64, to: f64, steps: u32 },
    /// The integers `from`, `from + 1`, … (`count` of them).
    Seeds { from: i64, count: u32 },
}

impl AxisValues {
    pub fn len(&self) -> usize {
        match self {
            Self::List(v) => v.len(),
            Self::Range { steps, .. } => *steps as usize,
            Self::Seeds { count, .. } => *count as usize,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Value `i` (`i < len`).
    pub fn get(&self, i: usize) -> Json {
        match self {
            Self::List(v) => v[i].clone(),
            Self::Range { from, to, steps } => {
                let t = if *steps <= 1 {
                    0.0
                } else {
                    i as f64 / f64::from(steps - 1)
                };
                Json::from(from + (to - from) * t)
            }
            Self::Seeds { from, .. } => Json::from(from + i as i64),
        }
    }
}

/// One axis: a parameter of a node and the values it takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    pub node: NodeId,
    pub param: String,
    /// Attribute name in the media bin; `None` means `<node name>.<param>`.
    pub name: Option<String>,
    pub values: AxisValues,
}

/// A wedge: work items over parameters of the main network, cooked for
/// one node and kept in the media bin.
#[derive(Debug, Clone, PartialEq)]
pub struct Wedge {
    pub target: NodeId,
    pub frame: u64,
    pub fps: f64,
    /// Output size; `None` is the project resolution.
    pub size: Option<[u32; 2]>,
    pub combine: Combine,
    pub scheduler: Scheduler,
    /// Items in flight per host.
    pub concurrency: u32,
    pub axes: Vec<Axis>,
}

impl Wedge {
    /// A wedge over no axes yet, run locally.
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            frame: 0,
            fps: 60.0,
            size: None,
            combine: Combine::Product,
            scheduler: Scheduler::default(),
            concurrency: 1,
            axes: Vec::new(),
        }
    }

    /// How many work items the wedge makes.
    pub fn len(&self) -> usize {
        if self.axes.is_empty() {
            return 0;
        }
        match self.combine {
            Combine::Product => self
                .axes
                .iter()
                .map(|a| a.values.len())
                .fold(1usize, usize::saturating_mul),
            Combine::Zip => self.axes.iter().map(|a| a.values.len()).min().unwrap_or(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Each work item's value per axis, in item order.
    pub fn items(&self) -> Vec<Vec<Json>> {
        let n = self.len();
        (0..n)
            .map(|i| match self.combine {
                Combine::Zip => self.axes.iter().map(|a| a.values.get(i)).collect(),
                Combine::Product => {
                    let mut rest = i;
                    let mut out = vec![Json::Null; self.axes.len()];
                    for (k, a) in self.axes.iter().enumerate().rev() {
                        let len = a.values.len();
                        out[k] = a.values.get(rest % len);
                        rest /= len;
                    }
                    out
                }
            })
            .collect()
    }

    /// Problems that make the wedge unrunnable.
    pub fn check(&self) -> Result<(), String> {
        if self.axes.is_empty() {
            return Err("the wedge has no axes".into());
        }
        for (i, a) in self.axes.iter().enumerate() {
            if a.values.is_empty() {
                return Err(format!("axis {} ({}) has no values", i + 1, a.param));
            }
        }
        if self.combine == Combine::Zip {
            let lens: Vec<usize> = self.axes.iter().map(|a| a.values.len()).collect();
            if lens.windows(2).any(|w| w[0] != w[1]) {
                return Err(format!(
                    "zip needs axes of one length; they have {}",
                    lens.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        let product = self
            .axes
            .iter()
            .map(|a| a.values.len())
            .try_fold(1usize, |acc, n| acc.checked_mul(n));
        if self.combine == Combine::Product && product.is_none_or(|n| n > MAX_ITEMS) {
            return Err(format!("the wedge makes more than {MAX_ITEMS} items"));
        }
        Ok(())
    }
}

/// A wedge name: lower-case letters, digits, `_` and `-` (its file name).
pub fn valid_wedge_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn int(v: u64) -> Result<Value, FormatError> {
    i64::try_from(v)
        .map(Value::from)
        .map_err(|_| invalid(format!("{v} is too large")))
}

/// A wedge as text.
pub fn wedge_to_string(w: &Wedge) -> Result<String, FormatError> {
    let mut doc = DocumentMut::new();
    doc.insert("format", Item::Value(Value::from(FORMAT)));
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(HEADER);
    }
    doc.insert("target", Item::Value(int(w.target.0)?));
    doc.insert("frame", Item::Value(int(w.frame)?));
    doc.insert("fps", Item::Value(Value::from(w.fps)));
    if let Some([x, y]) = w.size {
        let mut a = Array::new();
        a.push(i64::from(x));
        a.push(i64::from(y));
        doc.insert("size", Item::Value(Value::Array(a)));
    }
    doc.insert("combine", Item::Value(Value::from(w.combine.as_str())));
    let scheduler = match &w.scheduler {
        Scheduler::Lan => Value::from("lan:*"),
        Scheduler::Hosts(h) if h.len() == 1 => string(&h[0]),
        Scheduler::Hosts(h) => {
            let mut a = Array::new();
            for x in h {
                a.push(x.as_str());
            }
            Value::Array(a)
        }
    };
    doc.insert("scheduler", Item::Value(scheduler));
    doc.insert(
        "concurrency",
        Item::Value(Value::from(i64::from(w.concurrency))),
    );
    let mut list = ArrayOfTables::new();
    for a in &w.axes {
        let mut t = Table::new();
        t.insert("node", Item::Value(int(a.node.0)?));
        t.insert("param", Item::Value(string(&a.param)));
        if let Some(n) = &a.name {
            t.insert("name", Item::Value(string(n)));
        }
        match &a.values {
            AxisValues::List(v) => {
                let mut arr = Array::new();
                for x in v {
                    arr.push(
                        json_to_toml(x)
                            .map_err(|e| invalid(format!("axis {}.{}: {e}", a.node, a.param)))?,
                    );
                }
                t.insert("values", Item::Value(Value::Array(arr)));
            }
            AxisValues::Range { from, to, steps } => {
                let mut r = InlineTable::new();
                r.insert("from", Value::from(*from));
                r.insert("to", Value::from(*to));
                r.insert("steps", Value::from(i64::from(*steps)));
                t.insert("range", Item::Value(Value::InlineTable(r)));
            }
            AxisValues::Seeds { from, count } => {
                let mut r = InlineTable::new();
                r.insert("from", Value::from(*from));
                r.insert("count", Value::from(i64::from(*count)));
                t.insert("seeds", Item::Value(Value::InlineTable(r)));
            }
        }
        list.push(t);
    }
    if !list.is_empty() {
        doc.insert("axis", Item::ArrayOfTables(list));
    }
    Ok(doc.to_string())
}

fn number(item: &Item, at: &str) -> Result<f64, FormatError> {
    item.as_float()
        .or_else(|| item.as_integer().map(|i| i as f64))
        .filter(|f| f.is_finite())
        .ok_or_else(|| invalid(format!("{at} must be a number")))
}

fn count(t: &dyn TableLike, key: &str, at: &str, max: u64) -> Result<u32, FormatError> {
    let v = required_u64(t, key, at)?;
    if v == 0 || v > max {
        return Err(invalid(format!("{at}: `{key}` must be 1 to {max}")));
    }
    Ok(v as u32)
}

fn host(s: &str, at: &str) -> Result<String, FormatError> {
    if valid_batch_host(s) {
        Ok(s.to_owned())
    } else {
        Err(invalid(format!(
            "{at}: `{s}` is not a host: use local, lan:<name>, cloud:<name>, comfy:<host:port> or lan:*"
        )))
    }
}

fn scheduler(item: &Item) -> Result<Scheduler, FormatError> {
    let at = "`scheduler`";
    if let Some(s) = item.as_str() {
        return if s == "lan:*" {
            Ok(Scheduler::Lan)
        } else {
            Ok(Scheduler::Hosts(vec![host(s, at)?]))
        };
    }
    let a = item
        .as_array()
        .ok_or_else(|| invalid(format!("{at} must be a host or a list of hosts")))?;
    let mut hosts: Vec<String> = Vec::new();
    for v in a {
        let s = v
            .as_str()
            .ok_or_else(|| invalid(format!("{at}: hosts are strings")))?;
        let h = host(s, at)?;
        if hosts.contains(&h) {
            return Err(invalid(format!("{at}: `{h}` is listed twice")));
        }
        hosts.push(h);
    }
    if hosts.is_empty() {
        return Err(invalid(format!("{at}: the list is empty")));
    }
    Ok(Scheduler::Hosts(hosts))
}

fn axis(t: &dyn TableLike, i: usize) -> Result<Axis, FormatError> {
    let at = format!("axis {}", i + 1);
    only_keys(
        t,
        &["node", "param", "name", "values", "range", "seeds"],
        &at,
    )?;
    let node = NodeId(required_u64(t, "node", &at)?);
    let param = required_str(t, "param", &at)?.to_owned();
    let name = match t.get("name") {
        None => None,
        Some(_) => Some(required_str(t, "name", &at)?.to_owned()),
    };
    let given: Vec<&str> = ["values", "range", "seeds"]
        .into_iter()
        .filter(|k| t.contains_key(k))
        .collect();
    if given.len() != 1 {
        return Err(invalid(format!(
            "{at} ({param}): give exactly one of `values`, `range` and `seeds`"
        )));
    }
    let values = match given[0] {
        "values" => {
            let a = t
                .get("values")
                .and_then(Item::as_array)
                .ok_or_else(|| invalid(format!("{at}: `values` must be a list")))?;
            AxisValues::List(
                a.iter()
                    .map(|v| toml_to_json(v).map_err(|e| invalid(format!("{at}: {e}"))))
                    .collect::<Result<_, _>>()?,
            )
        }
        "range" => {
            let r = table_like(t.get("range").expect("given"), &format!("{at} `range`"))?;
            let rat = format!("{at} `range`");
            only_keys(r, &["from", "to", "steps"], &rat)?;
            let get = |k: &str| {
                r.get(k)
                    .ok_or_else(|| invalid(format!("{rat}: missing `{k}`")))
                    .and_then(|v| number(v, &format!("{rat} `{k}`")))
            };
            AxisValues::Range {
                from: get("from")?,
                to: get("to")?,
                steps: count(r, "steps", &rat, MAX_ITEMS as u64)?,
            }
        }
        _ => {
            let r = table_like(t.get("seeds").expect("given"), &format!("{at} `seeds`"))?;
            let rat = format!("{at} `seeds`");
            only_keys(r, &["from", "count"], &rat)?;
            let from = r
                .get("from")
                .and_then(Item::as_integer)
                .ok_or_else(|| invalid(format!("{rat}: `from` must be a whole number")))?;
            AxisValues::Seeds {
                from,
                count: count(r, "count", &rat, MAX_ITEMS as u64)?,
            }
        }
    };
    if values.is_empty() {
        return Err(invalid(format!("{at} ({param}): no values")));
    }
    Ok(Axis {
        node,
        param,
        name,
        values,
    })
}

/// Parses a wedge file.
pub fn wedge_from_str(source: &str) -> Result<Wedge, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(e.to_string().trim_end().to_owned()))?;
    let root = doc.as_table();
    only_keys(
        root,
        &[
            "format",
            "target",
            "frame",
            "fps",
            "size",
            "combine",
            "scheduler",
            "concurrency",
            "axis",
        ],
        "the file",
    )?;
    check_format(root)?;
    let mut w = Wedge::new(NodeId(required_u64(root, "target", "the file")?));
    if root.contains_key("frame") {
        w.frame = required_u64(root, "frame", "the file")?;
    }
    if let Some(v) = root.get("fps") {
        w.fps = number(v, "`fps`")?;
        if w.fps <= 0.0 {
            return Err(invalid("`fps` must be more than 0".into()));
        }
    }
    if let Some(item) = root.get("size") {
        let bad = || invalid("`size` must be [width, height], each 1 to 16384".into());
        let a = item.as_array().ok_or_else(bad)?;
        let v: Vec<u32> = a
            .iter()
            .map(|v| v.as_integer().and_then(|i| u32::try_from(i).ok()))
            .collect::<Option<_>>()
            .filter(|v: &Vec<u32>| v.len() == 2 && v.iter().all(|x| (1..=16_384).contains(x)))
            .ok_or_else(bad)?;
        w.size = Some([v[0], v[1]]);
    }
    if let Some(v) = root.get("combine") {
        w.combine = match v.as_str() {
            Some("product") => Combine::Product,
            Some("zip") => Combine::Zip,
            _ => return Err(invalid("`combine` must be \"product\" or \"zip\"".into())),
        };
    }
    if let Some(v) = root.get("scheduler") {
        w.scheduler = scheduler(v)?;
    }
    if root.contains_key("concurrency") {
        w.concurrency = count(root, "concurrency", "the file", u64::from(MAX_CONCURRENCY))?;
    }
    for (i, t) in tables(root.get("axis"), "axis")?.into_iter().enumerate() {
        w.axes.push(axis(t, i)?);
    }
    Ok(w)
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> FormatError + '_ {
    move |source| FormatError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The file of wedge `name` in project folder `dir`.
pub fn wedge_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(DIR).join(format!("{name}.toml"))
}

/// The project's wedge names, sorted.
pub fn list_wedges(dir: &Path) -> Result<Vec<String>, FormatError> {
    let folder = dir.join(DIR);
    if !folder.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&folder).map_err(io(&folder))? {
        let path = entry.map_err(io(&folder))?.path();
        if path.extension().is_some_and(|e| e == "toml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && valid_wedge_name(stem)
        {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Reads wedge `name` of the project at `dir`.
pub fn load_wedge(dir: &Path, name: &str) -> Result<Wedge, FormatError> {
    if !valid_wedge_name(name) {
        return Err(invalid(format!(
            "wedge `{name}`: names use a-z, 0-9, `_` and `-`"
        )));
    }
    let path = wedge_path(dir, name);
    let text = std::fs::read_to_string(&path).map_err(io(&path))?;
    let file = format!("{DIR}/{name}.toml");
    wedge_from_str(&text).map_err(|e| match e {
        FormatError::Invalid(message) => FormatError::InFile { file, message },
        other => other,
    })
}

/// Writes wedge `name` into the project at `dir`.
pub fn save_wedge(dir: &Path, name: &str, wedge: &Wedge) -> Result<(), FormatError> {
    if !valid_wedge_name(name) {
        return Err(invalid(format!(
            "wedge `{name}`: names use a-z, 0-9, `_` and `-`"
        )));
    }
    let folder = dir.join(DIR);
    std::fs::create_dir_all(&folder).map_err(io(&folder))?;
    crate::write_atomic(&wedge_path(dir, name), &wedge_to_string(wedge)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wedge() -> Wedge {
        let mut w = Wedge::new(NodeId(3));
        w.frame = 12;
        w.fps = 30.0;
        w.size = Some([512, 256]);
        w.scheduler = Scheduler::Lan;
        w.concurrency = 2;
        w.axes = vec![
            Axis {
                node: NodeId(3),
                param: "prompt1".into(),
                name: Some("prompt".into()),
                values: AxisValues::List(vec![json!("a red fox"), json!("two\nlines")]),
            },
            Axis {
                node: NodeId(3),
                param: "seed".into(),
                name: None,
                values: AxisValues::Seeds { from: 7, count: 3 },
            },
            Axis {
                node: NodeId(5),
                param: "colour".into(),
                name: None,
                values: AxisValues::List(vec![json!([1.0, 0.5, 0.0, 1.0])]),
            },
            Axis {
                node: NodeId(5),
                param: "strength".into(),
                name: None,
                values: AxisValues::Range {
                    from: 0.25,
                    to: 0.75,
                    steps: 3,
                },
            },
        ];
        w
    }

    #[test]
    fn wedges_round_trip_byte_stable() {
        let w = wedge();
        let text = wedge_to_string(&w).unwrap();
        let back = wedge_from_str(&text).unwrap();
        assert_eq!(back, w);
        assert_eq!(wedge_to_string(&back).unwrap(), text, "byte-stable");
        for s in [
            Scheduler::default(),
            Scheduler::Hosts(vec!["lan:a".into(), "comfy:127.0.0.1:8188".into()]),
            Scheduler::Hosts(vec!["cloud:runpod".into()]),
        ] {
            let mut w = wedge();
            w.scheduler = s;
            w.size = None;
            w.combine = Combine::Zip;
            let text = wedge_to_string(&w).unwrap();
            assert_eq!(wedge_from_str(&text).unwrap(), w, "{text}");
        }
    }

    #[test]
    fn a_hand_written_wedge_gets_defaults_and_canonical_form() {
        let text = "format = 1\ntarget = 4\n[[axis]]\nnode = 4\nparam = \"seed\"\nseeds = { count = 2, from = 1 }\n";
        let w = wedge_from_str(text).unwrap();
        assert_eq!(w.frame, 0);
        assert_eq!(w.fps, 60.0);
        assert_eq!(w.scheduler, Scheduler::Hosts(vec!["local".into()]));
        assert_eq!(w.concurrency, 1);
        let canon = wedge_to_string(&w).unwrap();
        assert!(canon.contains("seeds = { from = 1, count = 2 }"), "{canon}");
        assert!(canon.contains("scheduler = \"local\""), "{canon}");
    }

    #[test]
    fn products_vary_the_last_axis_fastest_and_zips_pair_up() {
        let mut w = wedge();
        assert_eq!(w.len(), 2 * 3 * 3);
        let items = w.items();
        assert_eq!(items.len(), 18);
        assert_eq!(
            items[0],
            [
                json!("a red fox"),
                json!(7),
                json!([1.0, 0.5, 0.0, 1.0]),
                json!(0.25)
            ]
        );
        assert_eq!(items[1][3], json!(0.5));
        assert_eq!(items[2][3], json!(0.75));
        assert_eq!(items[3][1], json!(8));
        assert_eq!(items[17][0], json!("two\nlines"));
        assert_eq!(items[17][1], json!(9));
        w.check().unwrap();
        w.combine = Combine::Zip;
        assert!(w.check().unwrap_err().contains("one length"));
        w.axes.truncate(2);
        w.axes[1].values = AxisValues::Seeds { from: 0, count: 2 };
        w.check().unwrap();
        assert_eq!(
            w.items(),
            [
                [json!("a red fox"), json!(0)],
                [json!("two\nlines"), json!(1)]
            ]
        );
    }

    #[test]
    fn bad_wedges_say_what_is_wrong() {
        for (text, want) in [
            ("format = 1", "`target`"),
            (
                "format = 1\ntarget = 1\nscheduler = \"ftp:x\"",
                "not a host",
            ),
            ("format = 1\ntarget = 1\nscheduler = []", "empty"),
            (
                "format = 1\ntarget = 1\nscheduler = [\"lan:a\", \"lan:a\"]",
                "twice",
            ),
            ("format = 1\ntarget = 1\nconcurrency = 0", "1 to 64"),
            ("format = 1\ntarget = 1\ncombine = \"cross\"", "product"),
            (
                "format = 1\ntarget = 1\n[[axis]]\nnode = 1\nparam = \"a\"\nvalues = [1]\nseeds = { from = 0, count = 1 }",
                "exactly one",
            ),
            (
                "format = 1\ntarget = 1\n[[axis]]\nnode = 1\nparam = \"a\"\nrange = { from = 0, to = 1, steps = 0 }",
                "`steps` must be 1",
            ),
            (
                "format = 1\ntarget = 1\n[[axis]]\nnode = 1\nparam = \"a\"\nvalues = []",
                "no values",
            ),
        ] {
            let err = wedge_from_str(text).unwrap_err().to_string();
            assert!(err.contains(want), "{text}: {err}");
        }
    }

    #[test]
    fn wedges_save_list_and_load_from_the_project() {
        let dir = std::env::temp_dir().join(format!("lsr-batch-fmt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_wedge(&dir, "seeds", &wedge()).unwrap();
        save_wedge(&dir, "a-first", &Wedge::new(NodeId(1))).unwrap();
        std::fs::write(dir.join(DIR).join("Not Valid.toml"), "").unwrap();
        assert_eq!(list_wedges(&dir).unwrap(), ["a-first", "seeds"]);
        assert_eq!(load_wedge(&dir, "seeds").unwrap(), wedge());
        assert!(save_wedge(&dir, "Bad", &wedge()).is_err());
        std::fs::write(wedge_path(&dir, "broken"), "format = 1").unwrap();
        let err = load_wedge(&dir, "broken").unwrap_err().to_string();
        assert!(err.starts_with("batch/broken.toml:"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
