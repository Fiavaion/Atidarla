//! Operator registry: schemas for every operator type, capability evaluation
//! and JSON export (consumed by the docs generator and the typed MCP tools).
//!
//! The parameter model here is the M0 subset needed for schemas; the full
//! parameter system (expressions, exports, animation) lands in M1.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::capability::{CapabilityFlag, CapabilityRequirement, Fallback};
use crate::platform::PlatformInfo;

/// The eight operator families (PRD A10, §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OpFamily {
    Img,
    Sig,
    Pnt,
    Msh,
    Mat,
    Dat,
    Cmp,
    Ai,
}

impl OpFamily {
    pub const ALL: [OpFamily; 8] = [
        OpFamily::Img,
        OpFamily::Sig,
        OpFamily::Pnt,
        OpFamily::Msh,
        OpFamily::Mat,
        OpFamily::Dat,
        OpFamily::Cmp,
        OpFamily::Ai,
    ];

    /// Lower-case prefix used in operator type IDs (`img.noise`).
    pub const fn prefix(self) -> &'static str {
        match self {
            OpFamily::Img => "img",
            OpFamily::Sig => "sig",
            OpFamily::Pnt => "pnt",
            OpFamily::Msh => "msh",
            OpFamily::Mat => "mat",
            OpFamily::Dat => "dat",
            OpFamily::Cmp => "cmp",
            OpFamily::Ai => "ai",
        }
    }
}

impl fmt::Display for OpFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.prefix().to_ascii_uppercase())
    }
}

/// Data types carried on wires (PRD §5.4). PNT and MSH share `Geo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum WireType {
    Img,
    Sig,
    Geo,
    Dat,
    Mat,
    Cmp,
}

/// An input or output socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketSchema {
    pub name: String,
    pub wire: WireType,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// Feedback input: reads the source's previous-frame result, so a wire
    /// into it may close a cycle (e.g. IMG Feedback).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub feedback: bool,
}

/// Parameter kinds (PRD §5.2 `atd-core`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamKind {
    Float {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    Int {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
    String,
    Menu {
        options: Vec<String>,
    },
    Toggle,
    Pulse,
    Colour,
    Vec {
        n: u8,
    },
    Path,
}

/// One parameter of an operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSchema {
    pub name: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: ParamKind,
    pub default: Value,
    /// One or two sentences for the docs page, tooltips and the assistant.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help: String,
    /// For string parameters: a list of values the UI can offer, by key
    /// (e.g. `audio.inputs`, `midi.inputs`). The value is still free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest: Option<String>,
    /// For string parameters: edited as several lines (text, code).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiline: bool,
}

impl ParamSchema {
    fn new(name: &str, label: &str, kind: ParamKind, default: Value) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            kind,
            default,
            help: String::new(),
            suggest: None,
            multiline: false,
        }
    }

    /// A float with optional hard limits.
    pub fn float(
        name: &str,
        label: &str,
        default: f64,
        min: Option<f64>,
        max: Option<f64>,
    ) -> Self {
        Self::new(
            name,
            label,
            ParamKind::Float { min, max },
            Value::from(default),
        )
    }

    pub fn int(name: &str, label: &str, default: i64, min: Option<i64>, max: Option<i64>) -> Self {
        Self::new(
            name,
            label,
            ParamKind::Int { min, max },
            Value::from(default),
        )
    }

    pub fn toggle(name: &str, label: &str, default: bool) -> Self {
        Self::new(name, label, ParamKind::Toggle, Value::from(default))
    }

    pub fn pulse(name: &str, label: &str) -> Self {
        Self::new(name, label, ParamKind::Pulse, Value::from(false))
    }

    pub fn string(name: &str, label: &str, default: &str) -> Self {
        Self::new(name, label, ParamKind::String, Value::from(default))
    }

    /// A file path; relative paths are relative to the project folder.
    pub fn path(name: &str, label: &str) -> Self {
        Self::new(name, label, ParamKind::Path, Value::from(""))
    }

    pub fn menu(name: &str, label: &str, options: &[&str], default: &str) -> Self {
        debug_assert!(options.contains(&default), "menu default must be an option");
        let options = options.iter().map(|o| (*o).to_owned()).collect();
        Self::new(
            name,
            label,
            ParamKind::Menu { options },
            Value::from(default),
        )
    }

    pub fn colour(name: &str, label: &str, rgba: [f64; 4]) -> Self {
        Self::new(name, label, ParamKind::Colour, Value::from(rgba.to_vec()))
    }

    pub fn vec(name: &str, label: &str, default: &[f64]) -> Self {
        let n = u8::try_from(default.len()).expect("vector parameters have at most 255 components");
        Self::new(
            name,
            label,
            ParamKind::Vec { n },
            Value::from(default.to_vec()),
        )
    }

    /// Adds help text.
    pub fn help(mut self, help: &str) -> Self {
        self.help = help.into();
        self
    }

    /// Offers the values listed under `key` in the UI (see [`Self::suggest`]).
    pub fn suggest(mut self, key: &str) -> Self {
        self.suggest = Some(key.into());
        self
    }

    /// Marks a string parameter as multi-line text.
    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// Checks a value against this parameter's kind. `min`/`max` are hard
    /// limits (they are also the JSON Schema `minimum`/`maximum`).
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        let numbers = |v: &Value, n: usize| {
            v.as_array().is_some_and(|a| {
                a.len() == n && a.iter().all(|x| x.as_f64().is_some_and(f64::is_finite))
            })
        };
        let ok = match &self.kind {
            ParamKind::Float { min, max } => value.as_f64().is_some_and(|v| {
                v.is_finite() && min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m)
            }),
            ParamKind::Int { min, max } => value
                .as_i64()
                .is_some_and(|v| min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m)),
            ParamKind::String | ParamKind::Path => value.is_string(),
            ParamKind::Menu { options } => value
                .as_str()
                .is_some_and(|s| options.iter().any(|o| o == s)),
            ParamKind::Toggle | ParamKind::Pulse => value.is_boolean(),
            ParamKind::Colour => numbers(value, 4),
            ParamKind::Vec { n } => numbers(value, usize::from(*n)),
        };
        if ok {
            Ok(())
        } else {
            Err(format!(
                "invalid value {value} for parameter `{}` ({:?})",
                self.name, self.kind
            ))
        }
    }

    /// JSON Schema fragment for this parameter's value.
    pub fn json_schema(&self) -> Value {
        let mut schema = match &self.kind {
            ParamKind::Float { min, max } => {
                let mut s = json!({ "type": "number" });
                if let Some(min) = min {
                    s["minimum"] = json!(min);
                }
                if let Some(max) = max {
                    s["maximum"] = json!(max);
                }
                s
            }
            ParamKind::Int { min, max } => {
                let mut s = json!({ "type": "integer" });
                if let Some(min) = min {
                    s["minimum"] = json!(min);
                }
                if let Some(max) = max {
                    s["maximum"] = json!(max);
                }
                s
            }
            ParamKind::String | ParamKind::Path => json!({ "type": "string" }),
            ParamKind::Menu { options } => json!({ "type": "string", "enum": options }),
            ParamKind::Toggle | ParamKind::Pulse => json!({ "type": "boolean" }),
            ParamKind::Colour => json!({
                "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4
            }),
            ParamKind::Vec { n } => json!({
                "type": "array", "items": { "type": "number" }, "minItems": n, "maxItems": n
            }),
        };
        schema["title"] = json!(self.label);
        if !self.help.is_empty() {
            schema["description"] = json!(self.help);
        }
        schema["default"] = self.default.clone();
        schema
    }
}

/// Complete schema of one operator type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorSchema {
    /// Stable type ID: `<family>.<name>`, lower-case, e.g. `ai.diffusion`.
    pub type_id: String,
    pub family: OpFamily,
    pub label: String,
    pub summary: String,
    pub version: u32,
    #[serde(default)]
    pub params: Vec<ParamSchema>,
    #[serde(default)]
    pub inputs: Vec<SocketSchema>,
    #[serde(default)]
    pub outputs: Vec<SocketSchema>,
    /// Platform-exclusive requirements (PRD A19); empty means "everywhere".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilityRequirement>,
    /// Cook-time budget in microseconds at the reference resolution.
    pub cook_budget_us: u32,
    /// Cooks every frame it is requested, even when nothing upstream changed
    /// (generators driven by time, live inputs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub time_dependent: bool,
    /// Has external side effects (network, devices, files), so the engine
    /// cooks it every frame even when nothing downstream is viewed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sink: bool,
}

impl OperatorSchema {
    /// JSON Schema (draft 2020-12) object describing this operator's parameters.
    pub fn params_json_schema(&self) -> Value {
        let properties: serde_json::Map<String, Value> = self
            .params
            .iter()
            .map(|p| (p.name.clone(), p.json_schema()))
            .collect();
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": self.label,
            "description": self.summary,
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        })
    }
}

/// Whether an operator can run natively on the current platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability<'a> {
    Native,
    Fallback {
        flag: &'a CapabilityFlag,
        fallback: &'a Fallback,
    },
}

/// Error registering an operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("operator `{0}` is already registered")]
    Duplicate(String),
    #[error("operator type ID `{0}` must be `<family>.<name>` in lower-case ASCII, digits or `_`")]
    InvalidTypeId(String),
    #[error("operator `{type_id}` is in family {family} but its ID has a different prefix")]
    FamilyMismatch { type_id: String, family: OpFamily },
    #[error("operator `{type_id}` declares parameter `{param}` twice")]
    DuplicateParam { type_id: String, param: String },
    #[error(
        "custom operator type ID `{0}` must be `py.<file>.<name>`, `kernel.<name>` or `comfy.<name>` in lower-case ASCII, digits or `_`"
    )]
    InvalidCustomId(String),
}

/// Provenance of operators a project defines in Python (`docs/python-ops.md`).
pub const PYTHON_CUSTOM: &str = "python.custom";

/// Provenance of operators a project defines as Kernel DSL files
/// (`kernels/<name>.atk`, `docs/kernel-dsl.md`).
pub const KERNEL_CUSTOM: &str = "kernel.custom";

/// Provenance of operators a project defines as ComfyUI API-format
/// workflows (`comfy/<name>.json`, `docs/comfy.md`).
pub const COMFY_CUSTOM: &str = "comfy.custom";

/// All registered operator types, ordered by type ID.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    ops: BTreeMap<String, OperatorSchema>,
    /// Where operators that are not built in come from (e.g.
    /// [`PYTHON_CUSTOM`]), by type ID.
    provenance: BTreeMap<String, String>,
}

/// Lower-case ASCII letters, digits and `_`, not empty.
fn valid_id_part(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Registry {
    pub fn register(&mut self, schema: OperatorSchema) -> Result<(), RegistryError> {
        let id = &schema.type_id;
        let (prefix, name) = id
            .split_once('.')
            .ok_or_else(|| RegistryError::InvalidTypeId(id.clone()))?;
        if !valid_id_part(name) {
            return Err(RegistryError::InvalidTypeId(id.clone()));
        }
        if prefix != schema.family.prefix() {
            return Err(RegistryError::FamilyMismatch {
                type_id: id.clone(),
                family: schema.family,
            });
        }
        self.insert(schema)
    }

    /// Registers or replaces an operator a project defines (custom code),
    /// recording where it comes from (e.g. [`PYTHON_CUSTOM`]). Its type ID is
    /// `py.<file>.<name>`, `kernel.<name>` or `comfy.<name>` whatever its
    /// family. Built-in
    /// operators cannot be replaced this way.
    pub fn replace_custom(
        &mut self,
        schema: OperatorSchema,
        provenance: &str,
    ) -> Result<(), RegistryError> {
        let id = schema.type_id.clone();
        let valid = match id.strip_prefix("py.") {
            Some(rest) => rest
                .split_once('.')
                .is_some_and(|(file, name)| valid_id_part(file) && valid_id_part(name)),
            None => id
                .strip_prefix("kernel.")
                .or_else(|| id.strip_prefix("comfy."))
                .is_some_and(valid_id_part),
        };
        if !valid {
            return Err(RegistryError::InvalidCustomId(id));
        }
        if self.ops.contains_key(&id) && !self.provenance.contains_key(&id) {
            return Err(RegistryError::Duplicate(id));
        }
        let old = self.ops.remove(&id);
        match self.insert(schema) {
            Ok(()) => {
                self.provenance.insert(id, provenance.into());
                Ok(())
            }
            Err(e) => {
                if let Some(old) = old {
                    self.ops.insert(id, old);
                }
                Err(e)
            }
        }
    }

    /// Removes an operator registered with [`Self::replace_custom`] (its file
    /// was removed, or another project opened). Load the network afterwards.
    pub fn remove_custom(&mut self, type_id: &str) -> Option<OperatorSchema> {
        self.provenance.remove(type_id)?;
        self.ops.remove(type_id)
    }

    /// Where a custom operator comes from; `None` for built-in operators.
    pub fn provenance(&self, type_id: &str) -> Option<&str> {
        self.provenance.get(type_id).map(String::as_str)
    }

    fn insert(&mut self, schema: OperatorSchema) -> Result<(), RegistryError> {
        let id = &schema.type_id;
        for (i, param) in schema.params.iter().enumerate() {
            if schema.params[..i].iter().any(|p| p.name == param.name) {
                return Err(RegistryError::DuplicateParam {
                    type_id: id.clone(),
                    param: param.name.clone(),
                });
            }
        }
        if self.ops.contains_key(id) {
            return Err(RegistryError::Duplicate(id.clone()));
        }
        self.ops.insert(id.clone(), schema);
        Ok(())
    }

    /// Registers `schema`, replacing an operator with the same type ID (a
    /// project's own shader taking over from another project's).
    pub fn replace(&mut self, schema: OperatorSchema) -> Result<(), RegistryError> {
        let old = self.ops.remove(&schema.type_id);
        let result = self.register(schema);
        if result.is_err()
            && let Some(old) = old
        {
            self.ops.insert(old.type_id.clone(), old);
        }
        result
    }

    pub fn get(&self, type_id: &str) -> Option<&OperatorSchema> {
        self.ops.get(type_id)
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Operators in type-ID order.
    pub fn iter(&self) -> impl Iterator<Item = &OperatorSchema> {
        self.ops.values()
    }

    /// Operators of one family, in type-ID order.
    pub fn family(&self, family: OpFamily) -> impl Iterator<Item = &OperatorSchema> {
        self.ops.values().filter(move |op| op.family == family)
    }

    /// Evaluates an operator's capability flags on `platform`. The first
    /// unsatisfied requirement decides the fallback.
    pub fn availability<'a>(
        schema: &'a OperatorSchema,
        platform: &PlatformInfo,
    ) -> Availability<'a> {
        schema
            .capabilities
            .iter()
            .find(|req| !req.flag.is_satisfied_by(platform))
            .map_or(Availability::Native, |req| Availability::Fallback {
                flag: &req.flag,
                fallback: &req.fallback,
            })
    }

    /// Deterministic JSON export of the whole registry, with a JSON Schema
    /// for each operator's parameters.
    pub fn export_json(&self) -> Value {
        let ops: Vec<Value> = self
            .ops
            .values()
            .map(|op| {
                let mut v = serde_json::to_value(op).expect("operator schema serialises");
                v["params_schema"] = op.params_json_schema();
                if let Some(p) = self.provenance(&op.type_id) {
                    v["provenance"] = json!(p);
                }
                v
            })
            .collect();
        json!({ "atidarla": crate::VERSION, "operators": ops })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{GpuVendor, Os};

    fn diffusion_fixture() -> OperatorSchema {
        OperatorSchema {
            type_id: "ai.diffusion".into(),
            family: OpFamily::Ai,
            label: "Diffusion".into(),
            summary: "Test fixture shaped like the Diffusion operator.".into(),
            version: 1,
            params: vec![
                ParamSchema {
                    name: "prompt".into(),
                    label: "Prompt".into(),
                    kind: ParamKind::String,
                    default: json!(""),
                    help: String::new(),
                    suggest: None,
                    multiline: false,
                },
                ParamSchema {
                    name: "strength".into(),
                    label: "Strength".into(),
                    kind: ParamKind::Float {
                        min: Some(0.0),
                        max: Some(1.0),
                    },
                    default: json!(0.5),
                    help: String::new(),
                    suggest: None,
                    multiline: false,
                },
            ],
            inputs: vec![SocketSchema {
                name: "image".into(),
                wire: WireType::Img,
                optional: true,
                feedback: false,
            }],
            outputs: vec![SocketSchema {
                name: "out".into(),
                wire: WireType::Img,
                optional: false,
                feedback: false,
            }],
            capabilities: vec![CapabilityRequirement {
                flag: "win+nvidia".parse().unwrap(),
                fallback: Fallback::RouteRemote {
                    explanation: "Real-time locally needs Windows + NVIDIA.".into(),
                },
            }],
            cook_budget_us: 500,
            time_dependent: false,
            sink: false,
        }
    }

    fn platform(os: Os, vendor: GpuVendor) -> PlatformInfo {
        PlatformInfo {
            os,
            gpu_vendor: vendor,
            ..PlatformInfo::host()
        }
    }

    #[test]
    fn availability_follows_capability_flags() {
        let op = diffusion_fixture();
        assert_eq!(
            Registry::availability(&op, &platform(Os::Windows, GpuVendor::Nvidia)),
            Availability::Native
        );
        match Registry::availability(&op, &platform(Os::MacOs, GpuVendor::Apple)) {
            Availability::Fallback { flag, fallback } => {
                assert_eq!(flag.to_string(), "win+nvidia");
                assert!(matches!(fallback, Fallback::RouteRemote { .. }));
            }
            other => panic!("expected fallback, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_registrations() {
        let mut reg = Registry::default();
        reg.register(diffusion_fixture()).unwrap();
        assert_eq!(
            reg.register(diffusion_fixture()),
            Err(RegistryError::Duplicate("ai.diffusion".into()))
        );

        let mut wrong_family = diffusion_fixture();
        wrong_family.family = OpFamily::Img;
        assert!(matches!(
            reg.register(wrong_family),
            Err(RegistryError::FamilyMismatch { .. })
        ));

        for bad in ["diffusion", "ai.", "ai.Diffusion", "ai.diff-usion"] {
            let mut op = diffusion_fixture();
            op.type_id = bad.into();
            assert!(
                matches!(reg.register(op), Err(RegistryError::InvalidTypeId(_))),
                "{bad}"
            );
        }

        let mut dup_param = diffusion_fixture();
        dup_param.type_id = "ai.other".into();
        dup_param.params.push(dup_param.params[0].clone());
        assert!(matches!(
            reg.register(dup_param),
            Err(RegistryError::DuplicateParam { .. })
        ));
    }

    #[test]
    fn export_is_deterministic_and_carries_json_schema() {
        let mut reg = Registry::default();
        let mut b = diffusion_fixture();
        b.type_id = "ai.zeta".into();
        reg.register(b).unwrap();
        reg.register(diffusion_fixture()).unwrap();

        let export = reg.export_json();
        let ids: Vec<&str> = export["operators"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["type_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["ai.diffusion", "ai.zeta"]);

        let op = &export["operators"][0];
        assert_eq!(op["capabilities"][0]["flag"], "win+nvidia");
        let strength = &op["params_schema"]["properties"]["strength"];
        assert_eq!(strength["type"], "number");
        assert_eq!(strength["maximum"], 1.0);
        assert_eq!(
            serde_json::to_string(&export).unwrap(),
            serde_json::to_string(&reg.export_json()).unwrap()
        );
    }

    #[test]
    fn builders_produce_valid_defaults() {
        let params = [
            ParamSchema::float("a", "A", 0.5, Some(0.0), Some(1.0)),
            ParamSchema::int("b", "B", 3, Some(1), None),
            ParamSchema::toggle("c", "C", true),
            ParamSchema::pulse("d", "D"),
            ParamSchema::string("e", "E", "hi"),
            ParamSchema::menu("f", "F", &["x", "y"], "y"),
            ParamSchema::colour("g", "G", [1.0, 0.5, 0.0, 1.0]),
            ParamSchema::vec("h", "H", &[0.0, 1.0]).help("A 2-vector."),
        ];
        for p in &params {
            p.validate(&p.default)
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
        }
        assert_eq!(params[7].json_schema()["description"], "A 2-vector.");
    }

    #[test]
    fn param_validation() {
        let op = diffusion_fixture();
        let strength = &op.params[1];
        assert!(strength.validate(&json!(0.25)).is_ok());
        assert!(strength.validate(&json!(1.5)).is_err());
        assert!(strength.validate(&json!("x")).is_err());
        let menu = ParamSchema {
            name: "mode".into(),
            label: "Mode".into(),
            kind: ParamKind::Menu {
                options: vec!["add".into(), "over".into()],
            },
            default: json!("over"),
            help: String::new(),
            suggest: None,
            multiline: false,
        };
        assert!(menu.validate(&json!("add")).is_ok());
        assert!(menu.validate(&json!("mul")).is_err());
        let v3 = ParamSchema {
            name: "t".into(),
            label: "T".into(),
            kind: ParamKind::Vec { n: 3 },
            default: json!([0, 0, 0]),
            help: String::new(),
            suggest: None,
            multiline: false,
        };
        assert!(v3.validate(&json!([1, 2.5, 3])).is_ok());
        assert!(v3.validate(&json!([1, 2])).is_err());
        let i = ParamSchema {
            name: "n".into(),
            label: "N".into(),
            kind: ParamKind::Int {
                min: Some(1),
                max: None,
            },
            default: json!(1),
            help: String::new(),
            suggest: None,
            multiline: false,
        };
        assert!(i.validate(&json!(4)).is_ok());
        assert!(i.validate(&json!(0)).is_err());
        assert!(i.validate(&json!(1.5)).is_err());
    }

    #[test]
    fn custom_operators_are_namespaced_and_carry_provenance() {
        let mut reg = Registry::default();
        reg.register(diffusion_fixture()).unwrap();
        let mut op = diffusion_fixture();
        op.type_id = "py.motion.counter".into();
        op.family = OpFamily::Sig;
        reg.replace_custom(op.clone(), PYTHON_CUSTOM).unwrap();
        assert_eq!(reg.provenance("py.motion.counter"), Some(PYTHON_CUSTOM));
        assert_eq!(reg.provenance("ai.diffusion"), None);
        assert_eq!(reg.family(OpFamily::Sig).count(), 1);
        // Replacing a custom operator is allowed (hot reload) ...
        op.label = "Counter 2".into();
        reg.replace_custom(op.clone(), PYTHON_CUSTOM).unwrap();
        assert_eq!(reg.get("py.motion.counter").unwrap().label, "Counter 2");
        let export = reg.export_json();
        assert_eq!(export["operators"][1]["provenance"], PYTHON_CUSTOM);
        // ... a built-in one is not, and IDs must be `py.<file>.<name>`.
        let mut builtin = diffusion_fixture();
        assert!(matches!(
            reg.replace_custom(builtin.clone(), PYTHON_CUSTOM),
            Err(RegistryError::InvalidCustomId(_))
        ));
        for bad in [
            "py.counter",
            "py.Motion.counter",
            "py.motion.",
            "sig.motion.x",
        ] {
            builtin.type_id = bad.into();
            assert!(
                reg.replace_custom(builtin.clone(), PYTHON_CUSTOM).is_err(),
                "{bad}"
            );
        }
        // A project kernel is `kernel.<name>` in any family.
        builtin.type_id = "kernel.curl".into();
        builtin.family = OpFamily::Pnt;
        reg.replace_custom(builtin.clone(), KERNEL_CUSTOM).unwrap();
        assert_eq!(reg.provenance("kernel.curl"), Some(KERNEL_CUSTOM));
        for bad in ["kernel.a.b", "kernel.", "kernel.Curl"] {
            builtin.type_id = bad.into();
            assert!(
                reg.replace_custom(builtin.clone(), KERNEL_CUSTOM).is_err(),
                "{bad}"
            );
        }
        // A project ComfyUI workflow is `comfy.<name>`.
        builtin.type_id = "comfy.invert".into();
        builtin.family = OpFamily::Ai;
        reg.replace_custom(builtin.clone(), COMFY_CUSTOM).unwrap();
        assert_eq!(reg.provenance("comfy.invert"), Some(COMFY_CUSTOM));
        for bad in ["comfy.a.b", "comfy.", "comfy.Invert"] {
            builtin.type_id = bad.into();
            assert!(
                reg.replace_custom(builtin.clone(), COMFY_CUSTOM).is_err(),
                "{bad}"
            );
        }
        assert!(reg.remove_custom("comfy.invert").is_some());
        assert!(reg.remove_custom("kernel.curl").is_some());
        assert_eq!(reg.remove_custom("ai.diffusion"), None, "built-in stays");
        assert!(reg.remove_custom("py.motion.counter").is_some());
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.provenance("py.motion.counter"), None);
    }

    #[test]
    fn schema_round_trips_through_json() {
        let op = diffusion_fixture();
        let back: OperatorSchema =
            serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
        assert_eq!(back, op);
    }
}
