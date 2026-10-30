//! Quest checks: typed conditions on a read-only [`Snapshot`] of the live
//! project (docs/learn.md). A check is a pure function of the snapshot, so
//! every kind is unit-tested without a GPU.

use std::path::{Component, Path};

use atd_core::graph::{Network, Node, NodeId};
use atd_core::registry::Registry;
use atd_format::Mappings;
use atd_stage::Stage;
use serde_json::Value;
use toml_edit::{Item, TableLike};

/// Prefixes of operators a project defines itself (Python, kernel files,
/// ComfyUI workflows, Pack instances). A quest may name them as `comfy.*`.
pub const PROJECT_PREFIXES: [&str; 4] = ["comfy.", "kernel.", "py.", "cmp.pack_"];

/// What the engine knows about a node's cooking.
pub trait CookInfo {
    /// How many times the node has cooked.
    fn cooks(&self, node: NodeId) -> u64;
    /// The node's last cook error.
    fn error(&self, node: NodeId) -> Option<&str>;
}

/// Everything a check may look at. Nothing in it is written to.
#[derive(Clone, Copy)]
pub struct Snapshot<'a> {
    pub network: &'a Network,
    pub registry: &'a Registry,
    pub cook: &'a dyn CookInfo,
    /// The Stage model of the network; `None` when the caller did not read
    /// it (see [`Check::needs_stage`]).
    pub stage: Option<&'a Stage>,
    pub mappings: &'a Mappings,
    /// Names of the Packs the open project's main network uses.
    pub pack_instances: &'a [String],
    /// Folder of the open project, if it has been saved or opened.
    pub project_dir: Option<&'a Path>,
}

impl std::fmt::Debug for Snapshot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("nodes", &self.network.len())
            .finish_non_exhaustive()
    }
}

/// An operator type id, or a prefix ending in `*` for operators a project
/// defines (`comfy.*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypePat(String);

impl TypePat {
    pub fn parse(s: &str) -> Result<Self, String> {
        let valid = |t: &str| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        };
        match s.strip_suffix('*') {
            Some(prefix) if PROJECT_PREFIXES.contains(&prefix) => Ok(Self(s.to_owned())),
            Some(prefix) => Err(format!(
                "`{prefix}*`: only {} can be a prefix",
                PROJECT_PREFIXES.join(", ")
            )),
            None if valid(s) => Ok(Self(s.to_owned())),
            None => Err(format!("`{s}` is not an operator type id")),
        }
    }

    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The exact type id, or `None` for a prefix.
    #[cfg(test)]
    pub fn exact(&self) -> Option<&str> {
        (!self.0.ends_with('*')).then_some(self.0.as_str())
    }

    pub fn matches(&self, type_id: &str) -> bool {
        match self.0.strip_suffix('*') {
            Some(prefix) => type_id.starts_with(prefix),
            None => type_id == self.0,
        }
    }
}

/// How a `param` check compares.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamOp {
    Gt(f64),
    Lt(f64),
    Eq(Value),
    /// Differs from the schema default.
    Changed,
    /// Follows a SIG channel.
    Bound,
    /// Driven by an expression.
    Expr,
    /// A string parameter containing this text.
    Contains(String),
}

/// A condition on the live project.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// A node of this type exists.
    NodeExists { ty: TypePat },
    /// A wire runs from a node of one type to a node of another, into
    /// `input` when given.
    Wired {
        from: TypePat,
        to: TypePat,
        input: Option<String>,
    },
    /// A node of this type has a parameter that passes `op`.
    Param {
        ty: TypePat,
        name: String,
        op: ParamOp,
    },
    /// A node of this type exists, has cooked and reports no error.
    CooksWithoutError { ty: TypePat },
    /// A Stage layer is playing a clip.
    StageLayerPlaying,
    /// A mapping drives this parameter of a node that exists.
    MappingExists { param: String },
    /// Every kernel in the patch compiles, and there is one.
    KernelCompiles,
    /// The open project uses a Pack of this name.
    PackInstance { name: String },
    /// A file exists in the open project's folder.
    FileExists { path: String },
}

const KINDS: [&str; 9] = [
    "node_exists",
    "wired",
    "param",
    "cooks_without_error",
    "stage_layer_playing",
    "mapping_exists",
    "kernel_compiles",
    "pack_instance",
    "file_exists",
];

impl Check {
    /// Whether [`Snapshot::stage`] must be filled for this check.
    pub fn needs_stage(&self) -> bool {
        matches!(self, Check::StageLayerPlaying)
    }

    /// Reads a check from a quest file's inline table.
    pub fn parse(table: &dyn TableLike) -> Result<Self, String> {
        let kind = text(table, "kind")?;
        let mut keys = Keys::new(table);
        keys.allow("kind");
        let check = match kind.as_str() {
            "node_exists" => Check::NodeExists {
                ty: keys.pat(table, "type")?,
            },
            "wired" => Check::Wired {
                from: keys.pat(table, "from_type")?,
                to: keys.pat(table, "to_type")?,
                input: keys.optional(table, "input")?,
            },
            "param" => Check::Param {
                ty: keys.pat(table, "type")?,
                name: keys.required(table, "name")?,
                op: ParamOp::parse(&mut keys, table)?,
            },
            "cooks_without_error" => Check::CooksWithoutError {
                ty: keys.pat(table, "type")?,
            },
            "stage_layer_playing" => Check::StageLayerPlaying,
            "mapping_exists" => Check::MappingExists {
                param: keys.required(table, "param")?,
            },
            "kernel_compiles" => Check::KernelCompiles,
            "pack_instance" => Check::PackInstance {
                name: keys.required(table, "name")?,
            },
            "file_exists" => {
                let path: String = keys.required(table, "path")?;
                let p = Path::new(&path);
                if path.is_empty()
                    || p.is_absolute()
                    || p.components().any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err(format!(
                        "file_exists: `{path}` must be a plain path inside the project"
                    ));
                }
                Check::FileExists { path }
            }
            other => {
                return Err(format!(
                    "unknown check kind `{other}` (known: {})",
                    KINDS.join(", ")
                ));
            }
        };
        keys.finish(&kind)?;
        Ok(check)
    }

    /// Whether the condition holds in `s`.
    pub fn eval(&self, s: &Snapshot<'_>) -> bool {
        let net = s.network;
        match self {
            Check::NodeExists { ty } => nodes(net, ty).next().is_some(),
            Check::Wired { from, to, input } => net.wires().any(|w| {
                input.as_deref().is_none_or(|i| w.to.socket == i)
                    && net
                        .node(w.from.node)
                        .is_some_and(|n| from.matches(&n.type_id))
                    && net.node(w.to.node).is_some_and(|n| to.matches(&n.type_id))
            }),
            Check::Param { ty, name, op } => nodes(net, ty).any(|n| op.holds(s, n, name)),
            Check::CooksWithoutError { ty } => {
                let mut any = false;
                for n in nodes(net, ty) {
                    if s.cook.error(n.id).is_some() {
                        return false;
                    }
                    any |= s.cook.cooks(n.id) > 0;
                }
                any
            }
            Check::StageLayerPlaying => s.stage.is_some_and(|st| {
                st.decks.iter().any(|d| {
                    d.layers
                        .iter()
                        .any(|l| l.target.is_some_and(|t| l.clip(t).is_some()))
                })
            }),
            Check::MappingExists { param } => s
                .mappings
                .mappings
                .iter()
                .any(|m| m.target.param == *param && net.node(m.target.node).is_some()),
            Check::KernelCompiles => kernels_compile(s),
            Check::PackInstance { name } => s.pack_instances.iter().any(|p| p == name),
            Check::FileExists { path } => s.project_dir.is_some_and(|d| d.join(path).is_file()),
        }
    }
}

fn nodes<'a>(net: &'a Network, ty: &'a TypePat) -> impl Iterator<Item = &'a Node> {
    net.nodes().filter(move |n| ty.matches(&n.type_id))
}

impl ParamOp {
    fn parse(keys: &mut Keys, table: &dyn TableLike) -> Result<Self, String> {
        let op: String = keys.required(table, "op")?;
        let value = |keys: &mut Keys| -> Result<&Item, String> {
            keys.allow("value");
            table
                .get("value")
                .ok_or_else(|| format!("param op `{op}` needs a `value`"))
        };
        let number = |item: &Item| {
            item.as_float()
                .or_else(|| item.as_integer().map(|i| i as f64))
                .ok_or_else(|| format!("param op `{op}` needs a number `value`"))
        };
        Ok(match op.as_str() {
            "gt" => ParamOp::Gt(number(value(keys)?)?),
            "lt" => ParamOp::Lt(number(value(keys)?)?),
            "eq" => {
                let item = value(keys)?;
                let v = if let Some(s) = item.as_str() {
                    Value::from(s)
                } else if let Some(b) = item.as_bool() {
                    Value::from(b)
                } else if let Some(i) = item.as_integer() {
                    Value::from(i)
                } else {
                    Value::from(number(item)?)
                };
                ParamOp::Eq(v)
            }
            "contains" => {
                let item = value(keys)?;
                ParamOp::Contains(
                    item.as_str()
                        .ok_or("param op `contains` needs a string `value`")?
                        .to_owned(),
                )
            }
            "changed" => ParamOp::Changed,
            "bound" => ParamOp::Bound,
            "expr" => ParamOp::Expr,
            other => {
                return Err(format!(
                    "unknown param op `{other}` (gt, lt, eq, changed, bound, expr, contains)"
                ));
            }
        })
    }

    fn holds(&self, s: &Snapshot<'_>, node: &Node, name: &str) -> bool {
        let set = node.params.get(name);
        match self {
            ParamOp::Bound => set.is_some_and(|p| p.bind.is_some()),
            ParamOp::Expr => set.is_some_and(|p| p.expr.as_deref().is_some_and(|e| !e.is_empty())),
            ParamOp::Changed => {
                let Some(p) = set else { return false };
                let default = s
                    .registry
                    .get(&node.type_id)
                    .and_then(|sc| sc.params.iter().find(|q| q.name == name))
                    .map(|q| &q.default);
                default.is_none_or(|d| !same(d, &p.value))
            }
            ParamOp::Gt(x) => value(s, node, name)
                .and_then(Value::as_f64)
                .is_some_and(|v| v > *x),
            ParamOp::Lt(x) => value(s, node, name)
                .and_then(Value::as_f64)
                .is_some_and(|v| v < *x),
            ParamOp::Eq(want) => value(s, node, name).is_some_and(|v| same(v, want)),
            ParamOp::Contains(t) => value(s, node, name)
                .and_then(Value::as_str)
                .is_some_and(|v| v.contains(t.as_str())),
        }
    }
}

fn value<'a>(s: &Snapshot<'a>, node: &Node, name: &str) -> Option<&'a Value> {
    s.network.param_value(s.registry, node.id, name)
}

/// Equality that does not care whether a number is stored as 6 or 6.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Kernels in the patch: inline code compiles; file-based and project
/// kernels have cooked without an error.
fn kernels_compile(s: &Snapshot<'_>) -> bool {
    use atd_kernel::{CompileOptions, Target};
    let mut any = false;
    for n in s.network.nodes() {
        let target = match n.type_id.as_str() {
            "pnt.kernel" => Target::Points,
            "img.kernel" => Target::Pixels,
            t if t.starts_with("kernel.") => {
                any = true;
                if s.cook.error(n.id).is_some() || s.cook.cooks(n.id) == 0 {
                    return false;
                }
                continue;
            }
            _ => continue,
        };
        any = true;
        let text = |p: &str| value(s, n, p).and_then(Value::as_str).unwrap_or("");
        if !text("file").is_empty() {
            if s.cook.error(n.id).is_some() || s.cook.cooks(n.id) == 0 {
                return false;
            }
            continue;
        }
        let mut opts = CompileOptions::new(target);
        opts.validate = false;
        if atd_kernel::compile(text("code"), &opts).is_err() {
            return false;
        }
    }
    any
}

// ---- reading quest tables ----

fn text(table: &dyn TableLike, key: &str) -> Result<String, String> {
    table
        .get(key)
        .and_then(Item::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("check needs a string `{key}`"))
}

/// Tracks which keys a check read, so a misspelt key is an error.
struct Keys {
    seen: Vec<String>,
    all: Vec<String>,
}

impl Keys {
    fn new(table: &dyn TableLike) -> Self {
        Self {
            seen: Vec::new(),
            all: table.iter().map(|(k, _)| k.to_owned()).collect(),
        }
    }

    fn allow(&mut self, key: &str) {
        self.seen.push(key.to_owned());
    }

    fn required(&mut self, table: &dyn TableLike, key: &str) -> Result<String, String> {
        self.allow(key);
        text(table, key)
    }

    fn optional(&mut self, table: &dyn TableLike, key: &str) -> Result<Option<String>, String> {
        self.allow(key);
        match table.get(key) {
            None => Ok(None),
            Some(_) => text(table, key).map(Some),
        }
    }

    fn pat(&mut self, table: &dyn TableLike, key: &str) -> Result<TypePat, String> {
        TypePat::parse(&self.required(table, key)?)
    }

    fn finish(self, kind: &str) -> Result<(), String> {
        match self.all.iter().find(|k| !self.seen.contains(k)) {
            Some(k) => Err(format!("check `{kind}` has no key `{k}`")),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<Check, String> {
        let doc: toml_edit::DocumentMut = format!("c = {src}").parse().unwrap();
        Check::parse(doc["c"].as_table_like().unwrap())
    }

    #[test]
    fn every_kind_parses_and_unknown_things_fail() {
        for ok in [
            r#"{ kind = "node_exists", type = "img.noise" }"#,
            r#"{ kind = "wired", from_type = "img.noise", to_type = "img.level", input = "in" }"#,
            r#"{ kind = "param", type = "img.level", name = "gamma", op = "gt", value = 1 }"#,
            r#"{ kind = "param", type = "img.level", name = "gamma", op = "changed" }"#,
            r#"{ kind = "cooks_without_error", type = "comfy.*" }"#,
            r#"{ kind = "stage_layer_playing" }"#,
            r#"{ kind = "mapping_exists", param = "brightness" }"#,
            r#"{ kind = "kernel_compiles" }"#,
            r#"{ kind = "pack_instance", name = "beat_flash" }"#,
            r#"{ kind = "file_exists", path = "packs/beat_flash/help.md" }"#,
        ] {
            parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for (bad, why) in [
            (r#"{ kind = "teleport" }"#, "unknown check kind"),
            (r#"{ kind = "node_exists" }"#, "needs a string `type`"),
            (
                r#"{ kind = "node_exists", type = "img.noise", tpye = "x" }"#,
                "no key `tpye`",
            ),
            (
                r#"{ kind = "node_exists", type = "Img Noise" }"#,
                "not an operator type id",
            ),
            (
                r#"{ kind = "node_exists", type = "img.*" }"#,
                "can be a prefix",
            ),
            (
                r#"{ kind = "param", type = "a.b", name = "x", op = "gt" }"#,
                "needs a `value`",
            ),
            (
                r#"{ kind = "param", type = "a.b", name = "x", op = "gt", value = "s" }"#,
                "number",
            ),
            (
                r#"{ kind = "param", type = "a.b", name = "x", op = "near", value = 1 }"#,
                "unknown param op",
            ),
            (r#"{ kind = "file_exists", path = "../x" }"#, "plain path"),
            (
                r#"{ kind = "file_exists", path = "/etc/passwd" }"#,
                "plain path",
            ),
        ] {
            let e = parse(bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn type_patterns_match_exact_ids_and_project_prefixes() {
        let exact = TypePat::parse("img.noise").unwrap();
        assert!(exact.matches("img.noise") && !exact.matches("img.noise2"));
        let prefix = TypePat::parse("comfy.*").unwrap();
        assert!(prefix.matches("comfy.invert") && !prefix.matches("img.noise"));
        assert_eq!(exact.exact(), Some("img.noise"));
        assert_eq!(prefix.exact(), None);
    }
}
