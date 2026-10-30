//! ComfyUI over its HTTP and WebSocket API (FR-A10, D9).
//!
//! ComfyUI is GPL-3.0, so Atidarla never bundles, links or starts it: a
//! user runs a ComfyUI server (on this machine or the LAN) and Atidarla
//! talks to it over the network only (CLAUDE.md rule 3). Every connection
//! goes through [`crate::egress`]: the server is resolved once by the gate
//! and only the addresses it returns are connected to, so Sovereign mode
//! refuses an internet server before any packet is sent.
//!
//! This module is the protocol and the parameter rules; the operator, its
//! worker thread and the project files are `atd_ops_ai::comfy`
//! (`docs/comfy.md`). Everything here blocks: call it from worker threads
//! only, never the render thread (rule 1).
//!
//! - [`parse_object_info`]: `GET /object_info` into typed input specs.
//! - [`parse_workflow`] + [`derive`]: an API-format workflow into
//!   parameters (the exposure rule), image inputs and the output node.
//! - [`build_prompt`]: the workflow with parameter values substituted.
//! - [`Client`]: upload, queue, events, history, view.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::egress;

/// Where ComfyUI listens by default.
pub const DEFAULT_SERVER: &str = "127.0.0.1:8188";

/// A node whose title starts with this is exposed; when any node has such a
/// title, only those nodes are.
pub const ATD_TITLE: &str = "atd:";

/// The node class whose `image` input becomes an IMG input socket.
pub const LOAD_IMAGE: &str = "LoadImage";

/// Node classes whose first image becomes the IMG output.
pub const OUTPUT_CLASSES: [&str; 2] = ["SaveImage", "PreviewImage"];

/// Subfolder of ComfyUI's input folder that uploaded frames go to.
pub const UPLOAD_SUBFOLDER: &str = "atidarla";

/// How long to wait for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one HTTP read may take (object_info can be large).
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a WebSocket read waits before the run checks for cancellation.
const EVENT_WAIT: Duration = Duration::from_millis(200);
/// How often the history is polled as well as the events (a missed event,
/// or no WebSocket at all, still finishes the run).
const HISTORY_EVERY: Duration = Duration::from_millis(750);
/// How long the history may lag behind the "done" event.
const HISTORY_GRACE: Duration = Duration::from_secs(10);

/// What can go wrong talking to ComfyUI.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComfyError {
    /// The egress gate refused the server (Sovereign mode) or could not
    /// resolve it.
    #[error("{message}")]
    Egress { sovereign: bool, message: String },
    #[error("server not reachable at {server}: {why}")]
    Unreachable { server: String, why: String },
    /// The server answered with something unexpected.
    #[error("{0}")]
    Server(String),
    /// `POST /prompt` refused the workflow (validation errors per node).
    #[error("the server refused the workflow: {0}")]
    Rejected(String),
    /// A node failed while running.
    #[error("{0}")]
    Execution(String),
    #[error("the run was interrupted on the server")]
    Interrupted,
    #[error("cancelled")]
    Cancelled,
    /// A workflow or object_info file that cannot be read.
    #[error("{0}")]
    Format(String),
}

// ---------------------------------------------------------------- object_info

/// A widget input: one that holds a value rather than a link.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Int {
        default: i64,
        min: Option<i64>,
        max: Option<i64>,
        /// A seed (`control_after_generate`, or named `seed`/`noise_seed`).
        seed: bool,
    },
    Float {
        default: f64,
        min: Option<f64>,
        max: Option<f64>,
    },
    String {
        default: String,
        multiline: bool,
    },
    Boolean {
        default: bool,
    },
    Combo {
        options: Vec<String>,
        default: Option<String>,
    },
}

/// What an input of a node class takes.
#[derive(Debug, Clone, PartialEq)]
pub enum InputType {
    Widget(Widget),
    /// Only a link from another node's output of this type (`MODEL`, ...).
    Link(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputSpec {
    pub name: String,
    pub ty: InputType,
    pub required: bool,
}

/// One node class from `/object_info`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClassInfo {
    pub display_name: String,
    /// Required inputs, then optional ones, each in the server's order.
    /// Hidden inputs are left out.
    pub inputs: Vec<InputSpec>,
    pub outputs: Vec<String>,
    pub output_node: bool,
}

impl ClassInfo {
    pub fn input(&self, name: &str) -> Option<&InputSpec> {
        self.inputs.iter().find(|i| i.name == name)
    }
}

/// Node classes by name.
pub type ObjectInfo = BTreeMap<String, ClassInfo>;

/// An integer as i64, saturating (seeds are often declared up to 2⁶⁴−1).
fn int_of(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
        .or_else(|| {
            v.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64)
        })
}

fn input_type(name: &str, spec: &Value) -> InputType {
    let Some(arr) = spec.as_array() else {
        return InputType::Link("*".into());
    };
    let empty = Map::new();
    let opts = arr.get(1).and_then(Value::as_object).unwrap_or(&empty);
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| a.iter().map(combo_text).collect())
            .unwrap_or_default()
    };
    let default = opts.get("default");
    let ty = match arr.first() {
        Some(Value::Array(_)) => {
            return InputType::Widget(Widget::Combo {
                options: strings(arr.first()),
                default: default.map(combo_text),
            });
        }
        Some(Value::String(t)) => t.as_str(),
        _ => return InputType::Link("*".into()),
    };
    InputType::Widget(match ty {
        "INT" => Widget::Int {
            default: default.and_then(int_of).unwrap_or(0),
            min: opts.get("min").and_then(int_of),
            max: opts.get("max").and_then(int_of),
            seed: opts
                .get("control_after_generate")
                .is_some_and(|v| v.as_bool() != Some(false))
                || matches!(name, "seed" | "noise_seed"),
        },
        "FLOAT" => Widget::Float {
            default: default.and_then(Value::as_f64).unwrap_or(0.0),
            min: opts.get("min").and_then(Value::as_f64),
            max: opts.get("max").and_then(Value::as_f64),
        },
        "STRING" => Widget::String {
            default: default.and_then(Value::as_str).unwrap_or("").to_owned(),
            multiline: opts.get("multiline").and_then(Value::as_bool) == Some(true),
        },
        "BOOLEAN" => Widget::Boolean {
            default: default.and_then(Value::as_bool).unwrap_or(false),
        },
        "COMBO" => Widget::Combo {
            options: strings(opts.get("options")),
            default: default.map(combo_text),
        },
        other => return InputType::Link(other.to_owned()),
    })
}

/// A combo option as text (options are strings; numbers are shown as such).
fn combo_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn parse_class(name: &str, v: &Value) -> Result<ClassInfo, ComfyError> {
    let obj = v
        .as_object()
        .ok_or_else(|| ComfyError::Format(format!("object_info: `{name}` is not an object")))?;
    let input = obj.get("input").and_then(Value::as_object);
    let order = obj.get("input_order").and_then(Value::as_object);
    let mut inputs = Vec::new();
    for (section, required) in [("required", true), ("optional", false)] {
        let Some(specs) = input.and_then(|i| i.get(section)).and_then(Value::as_object) else {
            continue;
        };
        let mut names: Vec<&str> = order
            .and_then(|o| o.get(section))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|n| specs.contains_key(*n))
                    .collect()
            })
            .unwrap_or_default();
        let mut rest: Vec<&str> = specs
            .keys()
            .map(String::as_str)
            .filter(|n| !names.contains(n))
            .collect();
        rest.sort_unstable();
        names.extend(rest);
        for n in names {
            inputs.push(InputSpec {
                name: n.to_owned(),
                ty: input_type(n, &specs[n]),
                required,
            });
        }
    }
    Ok(ClassInfo {
        display_name: obj
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_owned(),
        inputs,
        outputs: obj
            .get("output")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(combo_text).collect())
            .unwrap_or_default(),
        output_node: obj.get("output_node").and_then(Value::as_bool) == Some(true),
    })
}

/// Parses `GET /object_info` (or a project's cached subset of it).
pub fn parse_object_info(v: &Value) -> Result<ObjectInfo, ComfyError> {
    let obj = v
        .as_object()
        .ok_or_else(|| ComfyError::Format("object_info is not a JSON object".into()))?;
    obj.iter()
        .map(|(name, class)| Ok((name.clone(), parse_class(name, class)?)))
        .collect()
}

// ------------------------------------------------------------------ workflow

/// One node of an API-format workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct WfNode {
    pub id: String,
    pub class_type: String,
    /// `_meta.title`, if set.
    pub title: Option<String>,
    pub inputs: Map<String, Value>,
}

impl WfNode {
    /// The `atd:` title without its prefix, if it has one.
    pub fn atd_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .and_then(|t| t.trim_start().strip_prefix(ATD_TITLE))
            .map(str::trim)
    }

    /// What people call it: its title (without `atd:`), else its class.
    pub fn name(&self) -> &str {
        match self.atd_title().or(self.title.as_deref()).map(str::trim) {
            Some(t) if !t.is_empty() => t,
            _ => &self.class_type,
        }
    }
}

/// An API-format workflow ("Export (API)" in ComfyUI).
#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    /// Sorted by [`node_order`].
    pub nodes: Vec<WfNode>,
    raw: Map<String, Value>,
}

impl Workflow {
    pub fn node(&self, id: &str) -> Option<&WfNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The workflow as JSON, as read.
    pub fn to_value(&self) -> Value {
        Value::Object(self.raw.clone())
    }

    /// The node classes it uses, sorted.
    pub fn classes(&self) -> BTreeSet<&str> {
        self.nodes.iter().map(|n| n.class_type.as_str()).collect()
    }
}

/// Node IDs in number order (`2` before `10`); others (`5:3` in a
/// subgraph) after, by text.
pub fn node_order(a: &str, b: &str) -> CmpOrdering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => CmpOrdering::Less,
        (Err(_), Ok(_)) => CmpOrdering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// `["4", 0]`: a link to output 0 of node 4.
pub fn is_link(v: &Value) -> bool {
    v.as_array().is_some_and(|a| {
        a.len() == 2 && a[0].is_string() && (a[1].is_u64() || a[1].is_i64())
    })
}

/// Reads an API-format workflow.
pub fn parse_workflow(v: &Value) -> Result<Workflow, ComfyError> {
    let obj = v
        .as_object()
        .ok_or_else(|| ComfyError::Format("the workflow is not a JSON object".into()))?;
    if obj.get("nodes").is_some_and(Value::is_array) && obj.contains_key("links") {
        return Err(ComfyError::Format(
            "this is a ComfyUI UI workflow; export it with \"Export (API)\" (Workflow menu) and import that file"
                .into(),
        ));
    }
    let mut nodes = Vec::with_capacity(obj.len());
    for (id, node) in obj {
        let bad = |what: &str| ComfyError::Format(format!("workflow node `{id}`: {what}"));
        let n = node.as_object().ok_or_else(|| bad("not an object"))?;
        let class_type = n
            .get("class_type")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("no class_type (is this an API-format workflow?)"))?;
        let inputs = match n.get("inputs") {
            Some(Value::Object(m)) => m.clone(),
            None => Map::new(),
            Some(_) => return Err(bad("inputs is not an object")),
        };
        nodes.push(WfNode {
            id: id.clone(),
            class_type: class_type.to_owned(),
            title: n
                .get("_meta")
                .and_then(|m| m.get("title"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            inputs,
        });
    }
    if nodes.is_empty() {
        return Err(ComfyError::Format("the workflow has no nodes".into()));
    }
    nodes.sort_by(|a, b| node_order(&a.id, &b.id));
    Ok(Workflow {
        nodes,
        raw: obj.clone(),
    })
}

// --------------------------------------------------------------- derivation

/// A workflow input exposed as an operator parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Parameter name: lower-case letters, digits and `_`.
    pub id: String,
    pub label: String,
    pub node: String,
    pub input: String,
    /// Kind, limits and default (the workflow's value).
    pub widget: Widget,
    /// For seeds: the name of the "randomise each run" toggle.
    pub random: Option<String>,
    pub help: String,
}

/// A `LoadImage` node fed from an IMG input socket.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInput {
    pub socket: String,
    pub node: String,
    pub label: String,
}

/// What an operator made from a workflow has.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Derived {
    pub params: Vec<Param>,
    pub images: Vec<ImageInput>,
    /// The node whose first image is the IMG output.
    pub output: Option<String>,
    /// Only `atd:` nodes are exposed.
    pub selective: bool,
    /// Node classes the object_info does not describe (their parameters
    /// were inferred from the workflow's values).
    pub unknown_classes: Vec<String>,
}

/// Lower-case ASCII letters and digits, runs of anything else as one `_`,
/// none at the ends (`5:3` → `5_3`).
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_owned()
}

/// `KSampler (Advanced)` → `ksampler_advanced`: a name that may start a
/// parameter name.
pub fn ident(s: &str) -> String {
    let t = slug(s);
    match t.chars().next() {
        None => "node".into(),
        Some(c) if c.is_ascii_digit() => format!("n{t}"),
        Some(_) => t.to_owned(),
    }
}

/// The widget for a workflow value with no usable object_info entry.
fn infer(name: &str, v: &Value) -> Option<Widget> {
    Some(match v {
        Value::Bool(b) => Widget::Boolean { default: *b },
        Value::Number(n) if n.is_i64() || n.is_u64() => Widget::Int {
            default: int_of(v)?,
            min: None,
            max: None,
            seed: matches!(name, "seed" | "noise_seed"),
        },
        Value::Number(n) => Widget::Float {
            default: n.as_f64()?,
            min: None,
            max: None,
        },
        Value::String(s) => Widget::String {
            default: s.clone(),
            multiline: s.contains('\n'),
        },
        _ => return None,
    })
}

/// The class's widget with the workflow's value as default, kept within
/// the declared limits.
fn with_value(w: &Widget, v: &Value) -> Widget {
    match w.clone() {
        Widget::Int {
            default,
            min,
            max,
            seed,
        } => {
            let mut d = int_of(v).unwrap_or(default);
            if let Some(m) = max {
                d = d.min(m);
            }
            if let Some(m) = min {
                d = d.max(m);
            }
            Widget::Int {
                default: d,
                min,
                max,
                seed,
            }
        }
        Widget::Float { default, min, max } => {
            let mut d = v.as_f64().unwrap_or(default);
            if let Some(m) = max {
                d = d.min(m);
            }
            if let Some(m) = min {
                d = d.max(m);
            }
            Widget::Float { default: d, min, max }
        }
        Widget::String { default, multiline } => Widget::String {
            default: v.as_str().map_or(default, str::to_owned),
            multiline,
        },
        Widget::Boolean { default } => Widget::Boolean {
            default: v.as_bool().unwrap_or(default),
        },
        Widget::Combo {
            mut options,
            default,
        } => {
            let d = match v {
                Value::Null => default,
                other => Some(combo_text(other)),
            }
            .or_else(|| options.first().cloned())
            .unwrap_or_default();
            if !options.contains(&d) {
                options.push(d.clone());
            }
            Widget::Combo {
                options,
                default: Some(d),
            }
        }
    }
}

/// Inputs of `node` that are parameters, in the class's order then by name.
fn widgets(node: &WfNode, class: Option<&ClassInfo>) -> Vec<(String, Widget)> {
    let rank = |name: &str| {
        class
            .and_then(|c| c.inputs.iter().position(|i| i.name == name))
            .unwrap_or(usize::MAX)
    };
    let mut names: Vec<&String> = node.inputs.keys().collect();
    names.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    let mut out = Vec::new();
    for name in names {
        let v = &node.inputs[name];
        if is_link(v) {
            continue;
        }
        if node.class_type == LOAD_IMAGE && matches!(name.as_str(), "image" | "upload") {
            continue;
        }
        let widget = match class.and_then(|c| c.input(name)).map(|i| &i.ty) {
            Some(InputType::Widget(w)) => Some(with_value(w, v)),
            // A literal where the class declares a link, or an input the
            // class does not list: go by the value.
            _ => infer(name, v),
        };
        if let Some(w) = widget {
            out.push((name.clone(), w));
        }
    }
    out
}

/// A name not in `used` (adding `_2`, `_3`, ... when needed), now used.
fn claim(base: String, used: &mut BTreeSet<String>) -> String {
    let mut id = base.clone();
    let mut n = 2;
    while used.contains(&id) {
        id = format!("{base}_{n}");
        n += 1;
    }
    used.insert(id.clone());
    id
}

/// The exposure rule (`docs/comfy.md`): every widget input of every node
/// becomes a parameter named `<node title or class>_<input>`; when any
/// node's title starts with `atd:`, only those nodes' inputs do. Names are
/// lower-case identifiers; a name two nodes would share gets the node ID
/// appended (`ksampler_seed_3`), as does one in `reserved`. Parameters are
/// ordered by node ID, then by the class's input order, so the same files
/// always give the same operator. Each `LoadImage` node is an IMG input
/// (`in0`, `in1`, ... by node ID) and the first `SaveImage`/`PreviewImage`
/// (an `atd:` one first) is the IMG output.
pub fn derive(wf: &Workflow, info: &ObjectInfo, reserved: &[&str]) -> Derived {
    let selective = wf.nodes.iter().any(|n| n.atd_title().is_some());
    let mut found = Vec::new();
    let mut unknown = BTreeSet::new();
    for node in &wf.nodes {
        let class = info.get(&node.class_type);
        if class.is_none() {
            unknown.insert(node.class_type.clone());
        }
        if selective && node.atd_title().is_none() {
            continue;
        }
        for (input, widget) in widgets(node, class) {
            found.push((node, input, widget));
        }
    }
    // Bases two nodes share, or that are reserved, get the node ID.
    let base = |node: &WfNode, input: &str| format!("{}_{}", ident(node.name()), ident(input));
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for (node, input, _) in &found {
        *count.entry(base(node, input)).or_default() += 1;
    }
    let mut used: BTreeSet<String> = reserved.iter().map(|s| (*s).to_owned()).collect();
    let mut params = Vec::with_capacity(found.len());
    for (node, input, widget) in found {
        let b = base(node, &input);
        let wanted = if count[&b] > 1 || used.contains(&b) {
            format!("{b}_{}", slug(&node.id))
        } else {
            b
        };
        let id = claim(wanted, &mut used);
        let random = matches!(widget, Widget::Int { seed: true, .. })
            .then(|| claim(format!("{id}_random"), &mut used));
        params.push(Param {
            label: format!("{} {}", node.name(), input.replace('_', " ")),
            help: format!(
                "`{input}` of node {} ({}{}).",
                node.id,
                node.class_type,
                match node.title.as_deref() {
                    Some(t) if t != node.class_type => format!(", \"{t}\""),
                    _ => String::new(),
                }
            ),
            id,
            node: node.id.clone(),
            input,
            widget,
            random,
        });
    }
    let images = wf
        .nodes
        .iter()
        .filter(|n| n.class_type == LOAD_IMAGE)
        .enumerate()
        .map(|(i, n)| ImageInput {
            socket: format!("in{i}"),
            node: n.id.clone(),
            label: format!("{} (node {})", n.name(), n.id),
        })
        .collect();
    let outputs: Vec<&WfNode> = wf
        .nodes
        .iter()
        .filter(|n| OUTPUT_CLASSES.contains(&n.class_type.as_str()))
        .collect();
    let output = outputs
        .iter()
        .find(|n| n.atd_title().is_some())
        .or(outputs.first())
        .map(|n| n.id.clone());
    Derived {
        params,
        images,
        output,
        selective,
        unknown_classes: unknown.into_iter().collect(),
    }
}

/// The workflow with `values` (node ID, input name, value) and uploaded
/// image names (node ID, name) substituted. Links and everything else are
/// kept as they are.
pub fn build_prompt(
    wf: &Workflow,
    values: &[(String, String, Value)],
    images: &[(String, String)],
) -> Value {
    let mut raw = wf.raw.clone();
    let mut set = |node: &str, input: &str, v: Value| {
        if let Some(inputs) = raw
            .get_mut(node)
            .and_then(|n| n.get_mut("inputs"))
            .and_then(Value::as_object_mut)
        {
            inputs.insert(input.to_owned(), v);
        }
    };
    for (node, input, v) in values {
        set(node, input, v.clone());
    }
    for (node, name) in images {
        set(node, "image", Value::from(name.as_str()));
    }
    Value::Object(raw)
}

/// The object_info entries for the classes `wf` uses, as written to a
/// project's `comfy/<name>.info.json`. The server's list of files in its
/// input folder (`LoadImage`'s `image` options) is left out: it says
/// nothing about the workflow and would leak file names into the project.
pub fn info_subset(wf: &Workflow, object_info: &Value) -> Value {
    let mut out = Map::new();
    for class in wf.classes() {
        let Some(mut entry) = object_info.get(class).cloned() else {
            continue;
        };
        if class == LOAD_IMAGE
            && let Some(spec) = entry.pointer_mut("/input/required/image")
            && let Some(Value::Array(_)) = spec.get(0)
        {
            spec[0] = json!([]);
        }
        out.insert(class.to_owned(), entry);
    }
    Value::Object(out)
}

// -------------------------------------------------------------------- client

/// `host:port` from what a user typed (`127.0.0.1:8188`, `http://box:8188/`,
/// `[::1]:8188`, `studio.local`).
pub fn parse_server(s: &str) -> Result<(String, u16), ComfyError> {
    let bad = || {
        ComfyError::Format(format!(
            "`{s}` is not a server address (host:port, e.g. {DEFAULT_SERVER})"
        ))
    };
    let t = s.trim();
    if t.starts_with("https://") {
        return Err(ComfyError::Format(format!(
            "`{s}`: https is not supported; ComfyUI is reached over plain HTTP on this machine or the LAN"
        )));
    }
    let t = t.strip_prefix("http://").unwrap_or(t).trim_end_matches('/');
    if t.is_empty() || t.contains('/') {
        return Err(bad());
    }
    let (host, port) = if let Some(rest) = t.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or_else(bad)?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad())?,
            None if after.is_empty() => 8188,
            None => return Err(bad()),
        };
        (h.to_owned(), port)
    } else {
        match t.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_owned(), p.parse().map_err(|_| bad())?),
            Some(_) => return Err(bad()),
            None => (t.to_owned(), 8188),
        }
    };
    if host.is_empty() || port == 0 {
        return Err(bad());
    }
    Ok((host, port))
}

/// An image file to upload into a `LoadImage` node.
#[derive(Debug, Clone)]
pub struct Upload {
    pub node: String,
    /// File name on the server (in [`UPLOAD_SUBFOLDER`]).
    pub filename: String,
    /// The file's bytes (PNG).
    pub bytes: Vec<u8>,
}

/// One run of a workflow.
#[derive(Debug, Clone)]
pub struct Job {
    /// From [`build_prompt`], without the uploaded names (they are set
    /// once the uploads are done).
    pub prompt: Value,
    pub uploads: Vec<Upload>,
    /// The node whose first image is the result.
    pub output: Option<String>,
}

/// Where a run is.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Uploading,
    Queued,
    Running {
        node: String,
        class: String,
        /// Nodes finished, of `total` to run (cached ones excluded).
        done: usize,
        total: usize,
        /// Steps within the node (a sampler's), if it reports them.
        step: Option<(u64, u64)>,
    },
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uploading => f.write_str("uploading input images"),
            Self::Queued => f.write_str("queued"),
            Self::Running {
                node,
                class,
                done,
                total,
                step,
            } => {
                let n = (done + 1).min((*total).max(1));
                write!(f, "running {class} (node {node}) {n}/{total}")?;
                if let Some((v, m)) = step {
                    write!(f, ", step {v}/{m}")?;
                }
                Ok(())
            }
        }
    }
}

/// What a finished run produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub prompt_id: String,
    /// The output node's first image file (PNG or JPEG bytes), if the job
    /// has an output node.
    pub image: Option<Vec<u8>>,
    pub seconds: f64,
}

/// A connection to one ComfyUI server, opened through the egress gate.
pub struct Client {
    server: String,
    host: String,
    port: u16,
    addrs: Vec<SocketAddr>,
    agent: ureq::Agent,
    client_id: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("server", &self.server)
            .field("addrs", &self.addrs)
            .finish_non_exhaustive()
    }
}

static CLIENTS: AtomicU64 = AtomicU64::new(0);

impl Client {
    /// Resolves `server` through the egress gate (Sovereign mode refuses an
    /// internet server here, before anything is sent). No connection is
    /// made yet.
    pub fn new(server: &str) -> Result<Self, ComfyError> {
        let (host, port) = parse_server(server)?;
        let addrs = egress::resolve(&host, port).map_err(|e| ComfyError::Egress {
            sovereign: matches!(e, egress::EgressError::Sovereign { .. }),
            message: e.to_string(),
        })?;
        let fixed = addrs.clone();
        // ureq connects only to the addresses the gate returned; the URL
        // (and so the Host header) keeps the name the user gave.
        let agent = ureq::AgentBuilder::new()
            .resolver(move |_: &str| -> io::Result<Vec<SocketAddr>> { Ok(fixed.clone()) })
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .timeout_write(READ_TIMEOUT)
            .redirects(0)
            .build();
        let n = CLIENTS.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        Ok(Self {
            server: server.trim().to_owned(),
            client_id: format!("atidarla-{}-{nanos:08x}-{n}", std::process::id()),
            host,
            port,
            addrs,
            agent,
        })
    }

    /// `host:port` as it goes in a URL.
    fn netloc(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.netloc())
    }

    fn http_error(&self, e: ureq::Error) -> ComfyError {
        match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                ComfyError::Server(format!("HTTP {code} from {}: {}", self.server, short(&body)))
            }
            ureq::Error::Transport(t) => match t.kind() {
                ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Io => {
                    ComfyError::Unreachable {
                        server: self.server.clone(),
                        why: std::error::Error::source(&t)
                            .map_or_else(|| t.to_string(), ToString::to_string),
                    }
                }
                _ => ComfyError::Server(format!("{}: {t}", self.server)),
            },
        }
    }

    fn json(&self, resp: ureq::Response) -> Result<Value, ComfyError> {
        let text = resp
            .into_string()
            .map_err(|e| ComfyError::Server(format!("{}: {e}", self.server)))?;
        serde_json::from_str(&text)
            .map_err(|e| ComfyError::Server(format!("{}: not JSON: {e}", self.server)))
    }

    fn get_json(&self, path: &str) -> Result<Value, ComfyError> {
        let resp = self
            .agent
            .get(&self.url(path))
            .call()
            .map_err(|e| self.http_error(e))?;
        self.json(resp)
    }

    /// `GET /object_info`: every node class the server has.
    pub fn object_info(&self) -> Result<Value, ComfyError> {
        // The whole catalogue can be several megabytes.
        let resp = self
            .agent
            .get(&self.url("/object_info"))
            .call()
            .map_err(|e| self.http_error(e))?;
        let mut text = String::new();
        resp.into_reader()
            .read_to_string(&mut text)
            .map_err(|e| ComfyError::Server(format!("{}: {e}", self.server)))?;
        serde_json::from_str(&text)
            .map_err(|e| ComfyError::Server(format!("{}: object_info is not JSON: {e}", self.server)))
    }

    /// `POST /upload/image`; returns the name a `LoadImage` node takes
    /// (`subfolder/name`).
    pub fn upload_image(&self, filename: &str, bytes: &[u8]) -> Result<String, ComfyError> {
        let boundary = format!("atidarla-{}", self.client_id);
        let mut body = Vec::with_capacity(bytes.len() + 512);
        let field = |body: &mut Vec<u8>, name: &str, value: &str| {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        };
        field(&mut body, "type", "input");
        field(&mut body, "subfolder", UPLOAD_SUBFOLDER);
        field(&mut body, "overwrite", "true");
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let resp = self
            .agent
            .post(&self.url("/upload/image"))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|e| self.http_error(e))?;
        let v = self.json(resp)?;
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ComfyError::Server(format!("upload: no name in {}", short(&v.to_string()))))?;
        Ok(match v.get("subfolder").and_then(Value::as_str) {
            Some(sub) if !sub.is_empty() => format!("{sub}/{name}"),
            _ => name.to_owned(),
        })
    }

    /// `POST /prompt`; returns the prompt ID.
    pub fn queue(&self, prompt: &Value) -> Result<String, ComfyError> {
        let body = json!({ "prompt": prompt, "client_id": self.client_id });
        let resp = match self
            .agent
            .post(&self.url("/prompt"))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
        {
            Ok(r) => r,
            Err(ureq::Error::Status(code, resp)) if (400..500).contains(&code) => {
                let text = resp.into_string().unwrap_or_default();
                return Err(match serde_json::from_str::<Value>(&text) {
                    Ok(v) => ComfyError::Rejected(rejection(&v, prompt)),
                    Err(_) => ComfyError::Rejected(format!("HTTP {code}: {}", short(&text))),
                });
            }
            Err(e) => return Err(self.http_error(e)),
        };
        let v = self.json(resp)?;
        if let Some(errors) = v.get("node_errors").and_then(Value::as_object)
            && !errors.is_empty()
        {
            return Err(ComfyError::Rejected(rejection(&v, prompt)));
        }
        v.get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ComfyError::Server(format!("no prompt_id in {}", short(&v.to_string()))))
    }

    /// `GET /history/{id}`: the entry once the prompt has finished.
    pub fn history(&self, id: &str) -> Result<Option<Value>, ComfyError> {
        let v = self.get_json(&format!("/history/{id}"))?;
        Ok(v.get(id).cloned())
    }

    /// `GET /view`: an output file (`{filename, subfolder, type}`).
    pub fn view(&self, file: &Value) -> Result<Vec<u8>, ComfyError> {
        let s = |k: &str| file.get(k).and_then(Value::as_str).unwrap_or("");
        let resp = self
            .agent
            .get(&self.url("/view"))
            .query("filename", s("filename"))
            .query("subfolder", s("subfolder"))
            .query("type", if s("type").is_empty() { "output" } else { s("type") })
            .call()
            .map_err(|e| self.http_error(e))?;
        let mut bytes = Vec::new();
        resp.into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| ComfyError::Server(format!("{}: {e}", self.server)))?;
        Ok(bytes)
    }

    /// The event stream (`/ws?clientId=`), or `None` when it cannot be
    /// opened (the run then follows the history alone).
    fn events(&self) -> Option<tungstenite::WebSocket<TcpStream>> {
        let stream = self
            .addrs
            .iter()
            .find_map(|a| TcpStream::connect_timeout(a, CONNECT_TIMEOUT).ok())?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT)).ok()?;
        let url = format!("ws://{}/ws?clientId={}", self.netloc(), self.client_id);
        let (ws, _) = tungstenite::client(url.as_str(), stream).ok()?;
        ws.get_ref().set_read_timeout(Some(EVENT_WAIT)).ok()?;
        Some(ws)
    }

    /// Runs `job`: uploads its images, queues the prompt and follows it to
    /// the end, reporting progress; then fetches the output image. Blocks
    /// until done, failed or `cancel` is set.
    pub fn run(
        &self,
        mut job: Job,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Outcome, ComfyError> {
        let started = Instant::now();
        if !job.uploads.is_empty() {
            progress(Progress::Uploading);
        }
        for up in &job.uploads {
            if cancel.load(Ordering::Acquire) {
                return Err(ComfyError::Cancelled);
            }
            let name = self.upload_image(&up.filename, &up.bytes)?;
            if let Some(inputs) = job
                .prompt
                .get_mut(&up.node)
                .and_then(|n| n.get_mut("inputs"))
                .and_then(Value::as_object_mut)
            {
                inputs.insert("image".into(), Value::from(name));
            }
        }
        // Listen before queueing, so no event of this prompt is missed.
        let mut ws = self.events();
        let id = self.queue(&job.prompt)?;
        progress(Progress::Queued);
        let mut state = RunState {
            total: job.prompt.as_object().map_or(0, Map::len),
            ..RunState::default()
        };
        let mut polled = Instant::now();
        let entry = loop {
            if cancel.load(Ordering::Acquire) {
                return Err(ComfyError::Cancelled);
            }
            let mut finished = false;
            match &mut ws {
                Some(socket) => match socket.read() {
                    Ok(tungstenite::Message::Text(t)) => {
                        match state.handle(t.as_str(), &id, &job.prompt) {
                            Some(Step::Progress) => progress(state.progress(&job.prompt)),
                            Some(Step::Done) => finished = true,
                            Some(Step::Failed(e)) => return Err(e),
                            None => {}
                        }
                    }
                    // Binary frames are previews.
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(e) => {
                        log::debug!("ComfyUI events from {} ended: {e}", self.server);
                        ws = None;
                    }
                },
                None => std::thread::sleep(EVENT_WAIT),
            }
            if finished || polled.elapsed() >= HISTORY_EVERY {
                polled = Instant::now();
                if let Some(entry) = self.history(&id)? {
                    if let Some(e) = history_error(&entry) {
                        return Err(e);
                    }
                    if finished || history_done(&entry) {
                        break entry;
                    }
                }
                if finished && started.elapsed() > HISTORY_GRACE {
                    // Done by the events, but the history never came.
                    break Value::Null;
                }
            }
        };
        let image = match &job.output {
            None => None,
            Some(node) => {
                let file = entry
                    .pointer(&format!("/outputs/{}/images/0", pointer_escape(node)))
                    .ok_or_else(|| {
                        ComfyError::Execution(format!(
                            "the run finished but node {node} saved no image"
                        ))
                    })?;
                Some(self.view(file)?)
            }
        };
        Ok(Outcome {
            prompt_id: id,
            image,
            seconds: started.elapsed().as_secs_f64(),
        })
    }
}

/// `a/b~c` as a JSON pointer token.
fn pointer_escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// At most 300 characters of `s`, for messages.
fn short(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// "node 3 (KSampler): Value 2.0 bigger than max of 1.0: denoise" from a
/// `/prompt` error body.
fn rejection(v: &Value, prompt: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(nodes) = v.get("node_errors").and_then(Value::as_object) {
        let mut ids: Vec<&String> = nodes.keys().collect();
        ids.sort_by(|a, b| node_order(a, b));
        for id in ids {
            let n = &nodes[id];
            let class = n
                .get("class_type")
                .and_then(Value::as_str)
                .or_else(|| prompt.get(id)?.get("class_type")?.as_str())
                .unwrap_or("?");
            let msgs: Vec<String> = n
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|e| {
                    let m = e.get("message").and_then(Value::as_str).unwrap_or("error");
                    match e.get("details").and_then(Value::as_str) {
                        Some(d) if !d.is_empty() => format!("{m}: {d}"),
                        _ => m.to_owned(),
                    }
                })
                .collect();
            parts.push(format!("node {id} ({class}): {}", msgs.join("; ")));
        }
    }
    if parts.is_empty() {
        let e = v.get("error");
        let m = e
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .or_else(|| e.and_then(Value::as_str))
            .unwrap_or("unknown error");
        let d = e
            .and_then(|e| e.get("details"))
            .and_then(Value::as_str)
            .unwrap_or("");
        parts.push(if d.is_empty() {
            m.to_owned()
        } else {
            format!("{m}: {d}")
        });
    }
    parts.join("; ")
}

fn execution_error(d: &Value) -> ComfyError {
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("?");
    ComfyError::Execution(format!(
        "node {} ({}) failed: {}",
        d.get("node_id").map_or_else(|| "?".into(), combo_text),
        s("node_type"),
        s("exception_message").trim()
    ))
}

fn history_done(entry: &Value) -> bool {
    entry
        .pointer("/status/completed")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| entry.get("outputs").is_some_and(|o| o.is_object()))
}

fn history_error(entry: &Value) -> Option<ComfyError> {
    if entry.pointer("/status/status_str").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let messages = entry.pointer("/status/messages").and_then(Value::as_array);
    for m in messages.into_iter().flatten() {
        match m.get(0).and_then(Value::as_str) {
            Some("execution_error") => return Some(execution_error(&m[1])),
            Some("execution_interrupted") => return Some(ComfyError::Interrupted),
            _ => {}
        }
    }
    Some(ComfyError::Execution("the run failed on the server".into()))
}

/// Follows one prompt through the event stream.
#[derive(Debug, Default)]
struct RunState {
    total: usize,
    cached: usize,
    done: BTreeSet<String>,
    current: Option<String>,
    step: Option<(u64, u64)>,
}

enum Step {
    Progress,
    Done,
    Failed(ComfyError),
}

impl RunState {
    fn handle(&mut self, text: &str, id: &str, prompt: &Value) -> Option<Step> {
        let msg: Value = serde_json::from_str(text).ok()?;
        let data = msg.get("data")?;
        // Events of other prompts (another client's) are not ours.
        if let Some(p) = data.get("prompt_id").and_then(Value::as_str)
            && p != id
        {
            return None;
        }
        let ours = data.get("prompt_id").and_then(Value::as_str) == Some(id);
        match msg.get("type")?.as_str()? {
            "execution_start" if ours => Some(Step::Progress),
            "execution_cached" if ours => {
                self.cached = data.get("nodes").and_then(Value::as_array).map_or(0, Vec::len);
                Some(Step::Progress)
            }
            "executing" => match data.get("node").and_then(|n| n.as_str().map(str::to_owned)) {
                None if ours => Some(Step::Done),
                None => None,
                Some(node) => {
                    if let Some(prev) = self.current.replace(node.clone())
                        && prev != node
                    {
                        self.done.insert(prev);
                    }
                    self.step = None;
                    prompt.get(&node).map(|_| Step::Progress)
                }
            },
            "progress" => {
                let v = data.get("value").and_then(Value::as_u64)?;
                let m = data.get("max").and_then(Value::as_u64)?;
                if let Some(node) = data.get("node").and_then(Value::as_str) {
                    self.current = Some(node.to_owned());
                }
                self.step = Some((v, m));
                Some(Step::Progress)
            }
            "progress_state" if ours => {
                let nodes = data.get("nodes").and_then(Value::as_object)?;
                for (nid, s) in nodes {
                    match s.get("state").and_then(Value::as_str) {
                        Some("finished") => {
                            self.done.insert(nid.clone());
                        }
                        Some("running") => {
                            self.current = Some(nid.clone());
                            self.step = match (
                                s.get("value").and_then(Value::as_f64),
                                s.get("max").and_then(Value::as_f64),
                            ) {
                                (Some(v), Some(m)) if m > 1.0 => Some((v as u64, m as u64)),
                                _ => None,
                            };
                        }
                        _ => {}
                    }
                }
                Some(Step::Progress)
            }
            "executed" if ours => {
                if let Some(node) = data.get("node").and_then(Value::as_str) {
                    self.done.insert(node.to_owned());
                }
                Some(Step::Progress)
            }
            "execution_success" if ours => Some(Step::Done),
            "execution_error" if ours => Some(Step::Failed(execution_error(data))),
            "execution_interrupted" if ours => Some(Step::Failed(ComfyError::Interrupted)),
            _ => None,
        }
    }

    fn progress(&self, prompt: &Value) -> Progress {
        let Some(node) = &self.current else {
            return Progress::Queued;
        };
        let class = prompt
            .get(node)
            .and_then(|n| n.get("class_type"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        let done = self.done.iter().filter(|n| *n != node).count();
        Progress::Running {
            node: node.clone(),
            class: class.to_owned(),
            done,
            total: self.total.saturating_sub(self.cached).max(done + 1),
            step: self.step,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ObjectInfo {
        parse_object_info(&json!({
            "KSampler": {
                "input": {"required": {
                    "model": ["MODEL", {}],
                    "seed": ["INT", {"default": 0, "min": 0, "max": 18446744073709551615u64, "control_after_generate": true}],
                    "steps": ["INT", {"default": 20, "min": 1, "max": 10000}],
                    "cfg": ["FLOAT", {"default": 8.0, "min": 0.0, "max": 100.0, "step": 0.1}],
                    "sampler_name": [["euler", "dpmpp_2m"], {}],
                    "scheduler": ["COMBO", {"options": ["normal", "karras"], "default": "normal"}],
                    "denoise": ["FLOAT", {"default": 1.0, "min": 0.0, "max": 1.0}]
                }},
                "input_order": {"required": ["model", "seed", "steps", "cfg", "sampler_name", "scheduler", "denoise"]},
                "output": ["LATENT"], "output_node": false, "display_name": "KSampler"
            },
            "CLIPTextEncode": {
                "input": {"required": {"text": ["STRING", {"multiline": true}], "clip": ["CLIP"]}},
                "output": ["CONDITIONING"]
            },
            "LoadImage": {
                "input": {"required": {"image": [["a.png", "b.png"], {"image_upload": true}]}},
                "output": ["IMAGE", "MASK"]
            },
            "SaveImage": {
                "input": {"required": {"images": ["IMAGE"], "filename_prefix": ["STRING", {"default": "ComfyUI"}]},
                          "hidden": {"prompt": "PROMPT"}},
                "output": [], "output_node": true
            }
        }))
        .unwrap()
    }

    fn workflow() -> Workflow {
        parse_workflow(&json!({
            "3": {"class_type": "KSampler", "inputs": {"model": ["4", 0], "seed": 42, "steps": 4, "cfg": 1.5,
                  "sampler_name": "euler", "scheduler": "karras", "denoise": 0.6, "positive": ["6", 0]}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat\nin space", "clip": ["4", 1]}, "_meta": {"title": "Prompt"}},
            "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["4", 1]}, "_meta": {"title": "Prompt"}},
            "10": {"class_type": "LoadImage", "inputs": {"image": "x.png", "upload": "image"}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "out"}}
        }))
        .unwrap()
    }

    #[test]
    fn object_info_parses_widgets_combos_and_links() {
        let info = info();
        let k = &info["KSampler"];
        let names: Vec<&str> = k.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            ["model", "seed", "steps", "cfg", "sampler_name", "scheduler", "denoise"]
        );
        assert_eq!(k.inputs[0].ty, InputType::Link("MODEL".into()));
        assert_eq!(
            k.inputs[1].ty,
            InputType::Widget(Widget::Int {
                default: 0,
                min: Some(0),
                max: Some(i64::MAX),
                seed: true
            })
        );
        // Both combo forms.
        assert_eq!(
            k.inputs[4].ty,
            InputType::Widget(Widget::Combo {
                options: vec!["euler".into(), "dpmpp_2m".into()],
                default: None
            })
        );
        assert_eq!(
            k.inputs[5].ty,
            InputType::Widget(Widget::Combo {
                options: vec!["normal".into(), "karras".into()],
                default: Some("normal".into())
            })
        );
        assert!(matches!(
            info["CLIPTextEncode"].input("text").unwrap().ty,
            InputType::Widget(Widget::String { multiline: true, .. })
        ));
        let save = &info["SaveImage"];
        assert!(save.output_node);
        assert!(save.input("prompt").is_none(), "hidden inputs are left out");
        assert!(parse_object_info(&json!([1])).is_err());
    }

    #[test]
    fn parameters_are_derived_deterministically() {
        let d = derive(&workflow(), &info(), &["server", "run", "auto_run"]);
        let ids: Vec<&str> = d.params.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "ksampler_seed",
                "ksampler_steps",
                "ksampler_cfg",
                "ksampler_sampler_name",
                "ksampler_scheduler",
                "ksampler_denoise",
                "prompt_text_6",
                "prompt_text_7",
                "saveimage_filename_prefix"
            ]
        );
        let seed = &d.params[0];
        assert_eq!(seed.random.as_deref(), Some("ksampler_seed_random"));
        assert_eq!(
            seed.widget,
            Widget::Int {
                default: 42,
                min: Some(0),
                max: Some(i64::MAX),
                seed: true
            }
        );
        assert!(matches!(
            &d.params[3].widget,
            Widget::Combo { default: Some(s), .. } if s == "euler"
        ));
        assert!(matches!(
            &d.params[6].widget,
            Widget::String { default, multiline: true } if default == "a cat\nin space"
        ));
        assert_eq!(d.params[6].label, "Prompt text");
        assert_eq!(
            d.images,
            [ImageInput {
                socket: "in0".into(),
                node: "10".into(),
                label: "LoadImage (node 10)".into()
            }]
        );
        assert_eq!(d.output.as_deref(), Some("9"));
        assert!(!d.selective && d.unknown_classes.is_empty());
        // The same files give the same operator.
        assert_eq!(d, derive(&workflow(), &info(), &["server", "run", "auto_run"]));
        // A reserved name gets the node ID.
        let d2 = derive(&workflow(), &info(), &["ksampler_seed"]);
        assert_eq!(d2.params[0].id, "ksampler_seed_3");
    }

    #[test]
    fn atd_titles_select_what_is_exposed() {
        let mut v = workflow().to_value();
        v["3"]["_meta"] = json!({"title": "atd: Sampler"});
        v["9"]["_meta"] = json!({"title": "atd:out"});
        let d = derive(&parse_workflow(&v).unwrap(), &info(), &[]);
        assert!(d.selective);
        let ids: Vec<&str> = d.params.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "sampler_seed",
                "sampler_steps",
                "sampler_cfg",
                "sampler_sampler_name",
                "sampler_scheduler",
                "sampler_denoise",
                "out_filename_prefix"
            ]
        );
        assert_eq!(d.params[0].label, "Sampler seed");
        // Unknown classes are inferred from their values.
        let d = derive(&workflow(), &ObjectInfo::new(), &[]);
        assert_eq!(d.unknown_classes.len(), 4);
        let seed = d.params.iter().find(|p| p.id == "ksampler_seed").unwrap();
        assert!(matches!(
            seed.widget,
            Widget::Int { default: 42, min: None, seed: true, .. }
        ));
    }

    #[test]
    fn prompts_substitute_values_and_keep_links() {
        let wf = workflow();
        let p = build_prompt(
            &wf,
            &[
                ("3".into(), "seed".into(), json!(7)),
                ("6".into(), "text".into(), json!("a dog")),
            ],
            &[("10".into(), "atidarla/in.png".into())],
        );
        assert_eq!(p["3"]["inputs"]["seed"], 7);
        assert_eq!(p["3"]["inputs"]["model"], json!(["4", 0]));
        assert_eq!(p["6"]["inputs"]["text"], "a dog");
        assert_eq!(p["6"]["inputs"]["clip"], json!(["4", 1]));
        assert_eq!(p["10"]["inputs"]["image"], "atidarla/in.png");
        assert_eq!(p["7"], wf.to_value()["7"]);
    }

    #[test]
    fn ui_workflows_and_servers_are_explained() {
        let e = parse_workflow(&json!({"nodes": [], "links": []})).unwrap_err();
        assert!(e.to_string().contains("Export (API)"), "{e}");
        assert_eq!(parse_server("127.0.0.1:8188").unwrap(), ("127.0.0.1".into(), 8188));
        assert_eq!(parse_server("http://box.local:9000/").unwrap(), ("box.local".into(), 9000));
        assert_eq!(parse_server("[::1]:81").unwrap(), ("::1".into(), 81));
        assert_eq!(parse_server("studio").unwrap(), ("studio".into(), 8188));
        assert!(parse_server("https://x:1").is_err());
        assert!(parse_server("a:b").is_err());
        assert_eq!(ident("KSampler (Advanced)"), "ksampler_advanced");
        assert_eq!(ident("3"), "n3");
        assert_eq!(ident("--"), "node");
        assert_eq!(node_order("10", "9"), CmpOrdering::Greater);
        assert_eq!(node_order("9", "5:1"), CmpOrdering::Less);
    }

    #[test]
    fn both_progress_forms_are_followed() {
        let prompt = workflow().to_value();
        let mut s = RunState {
            total: 5,
            ..RunState::default()
        };
        let ev = |t: &str, d: Value| json!({"type": t, "data": d}).to_string();
        assert!(matches!(
            s.handle(&ev("execution_cached", json!({"prompt_id": "p", "nodes": ["6"]})), "p", &prompt),
            Some(Step::Progress)
        ));
        s.handle(&ev("executing", json!({"prompt_id": "p", "node": "3"})), "p", &prompt);
        s.handle(&ev("progress", json!({"prompt_id": "p", "node": "3", "value": 2, "max": 4})), "p", &prompt);
        assert_eq!(
            s.progress(&prompt).to_string(),
            "running KSampler (node 3) 1/4, step 2/4"
        );
        s.handle(
            &ev(
                "progress_state",
                json!({"prompt_id": "p", "nodes": {
                    "3": {"state": "finished", "value": 4, "max": 4},
                    "9": {"state": "running", "value": 0, "max": 1}
                }}),
            ),
            "p",
            &prompt,
        );
        assert_eq!(s.progress(&prompt).to_string(), "running SaveImage (node 9) 2/4");
        // Other prompts' events are ignored; the end is ours.
        assert!(s.handle(&ev("executing", json!({"prompt_id": "q", "node": null})), "p", &prompt).is_none());
        assert!(matches!(
            s.handle(&ev("executing", json!({"prompt_id": "p", "node": null})), "p", &prompt),
            Some(Step::Done)
        ));
        let failed = s.handle(
            &ev(
                "execution_error",
                json!({"prompt_id": "p", "node_id": "3", "node_type": "KSampler", "exception_message": "out of memory\n"}),
            ),
            "p",
            &prompt,
        );
        match failed {
            Some(Step::Failed(e)) => {
                assert_eq!(e.to_string(), "node 3 (KSampler) failed: out of memory");
            }
            _ => panic!("an execution error fails the run"),
        }
    }

    #[test]
    fn rejections_name_the_node() {
        let prompt = workflow().to_value();
        let body = json!({
            "error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation"},
            "node_errors": {"3": {"errors": [{"message": "Value bigger than max", "details": "denoise: 2.0"}], "class_type": "KSampler"}}
        });
        assert_eq!(
            rejection(&body, &prompt),
            "node 3 (KSampler): Value bigger than max: denoise: 2.0"
        );
        assert_eq!(
            rejection(&json!({"error": {"message": "no outputs"}}), &prompt),
            "no outputs"
        );
    }

    #[test]
    fn the_cached_subset_drops_server_file_lists() {
        let full = json!({
            "LoadImage": {"input": {"required": {"image": [["secret.png"], {"image_upload": true}]}}},
            "SaveImage": {"input": {}},
            "Unused": {}
        });
        let sub = info_subset(&workflow(), &full);
        assert_eq!(
            sub,
            json!({
                "LoadImage": {"input": {"required": {"image": [[], {"image_upload": true}]}}},
                "SaveImage": {"input": {}}
            })
        );
    }
}
