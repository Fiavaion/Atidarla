//! The models a project needs and its AI settings, in `project.toml`
//! (FR-A7, FR-A8, FR-A12, PRD A61).
//!
//! ```toml
//! [ai]
//! commercial_safe_only = true
//! sovereign = false             # absent = true (Sovereign mode on)
//! spend_cap_usd = 5.0           # absent = 0: no cloud spending
//!
//! [[ai.hosts]]                  # LAN and cloud AI Hosts, in name order
//! name = "lan:studio"
//! addr = "192.168.1.20:7420"
//!
//! [[models]]
//! id = "sd-turbo"
//! licence_class = "restricted"
//! sha256 = "40ec4008…"          # the model's main file
//! ```
//!
//! Model files never live in a project: they are in the per-user model
//! cache (`lsr_ai::models`). The project only names them, so opening it on
//! another machine can say which are missing. `[ai]` holds only the
//! settings that differ from their defaults (so Sovereign mode, on by
//! default, is written only as `sovereign = false`), and is left out when
//! none do; `[[models]]` is written only when there are models, so projects
//! without AI keep their bytes. Hosts and models are written in name and ID
//! order.
//!
//! A project saved before Sovereign mode existed has no `sovereign` key and
//! opens with it on: such projects never sent AI traffic anywhere, so
//! keeping it that way is safe, and it is the default for new projects
//! (FR-A8). Host tokens are never in a project: they are per user, beside
//! the model cache (`lsr_ai::hosts`).

use std::collections::BTreeMap;
use std::fmt;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::FormatError;
use crate::network::{invalid, only_keys, required_str, table_like, tables};

/// How a model's licence lets a show use it (FR-A12). The text names
/// (`commercial-safe`, …) are what files, the CLI and the UI show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LicenceClass {
    /// Permissive (MIT, Apache-2.0, BSD): any use, commercial included.
    CommercialSafe,
    /// Commercial use with conditions (revenue caps, registration, use
    /// policies), e.g. the Stability AI Community Licence.
    Restricted,
    /// Research or personal use only.
    NonCommercial,
}

impl LicenceClass {
    pub const ALL: [Self; 3] = [Self::CommercialSafe, Self::Restricted, Self::NonCommercial];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommercialSafe => "commercial-safe",
            Self::Restricted => "restricted",
            Self::NonCommercial => "non-commercial",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Whether a "commercial-safe only" project may use it.
    pub fn is_commercial_safe(self) -> bool {
        self == Self::CommercialSafe
    }
}

impl fmt::Display for LicenceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A model the project needs, by catalogue ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPin {
    pub licence_class: LicenceClass,
    /// SHA-256 of the model's main file, lower-case hex: the exact weights
    /// the project was made with.
    pub sha256: String,
}

/// `[ai]`: project-wide AI settings.
#[derive(Debug, Clone, PartialEq)]
pub struct AiSettings {
    /// Hide and refuse models whose licence class is not commercial-safe.
    pub commercial_safe_only: bool,
    /// Sovereign mode (FR-A8): AI traffic stays on this machine and the
    /// LAN. On by default.
    pub sovereign: bool,
    /// Cloud spending allowed per session, in US dollars (FR-A7); 0 allows
    /// none.
    pub spend_cap_usd: f64,
    /// LAN and cloud AI Hosts by name (`lan:<x>` or `cloud:<x>`) to their
    /// `host:port` address. Their tokens are per user, not here.
    pub hosts: BTreeMap<String, String>,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            commercial_safe_only: false,
            sovereign: true,
            spend_cap_usd: 0.0,
            hosts: BTreeMap::new(),
        }
    }
}

/// A LAN or cloud host name: `lan:<x>` or `cloud:<x>`, where `<x>` uses
/// a-z, 0-9, `-`, `_` and `.` (at most 64 characters).
pub fn valid_host_name(name: &str) -> bool {
    let rest = name
        .strip_prefix("lan:")
        .or_else(|| name.strip_prefix("cloud:"));
    rest.is_some_and(|r| {
        !r.is_empty()
            && r.len() <= 64
            && r.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
            })
    })
}

/// A host address: `host:port` with a port number, no spaces.
pub fn valid_host_addr(addr: &str) -> bool {
    addr.rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0))
        && !addr.chars().any(char::is_whitespace)
}

/// A model ID: lower-case letters, digits, `-`, `_` and `.`, so it is a
/// portable folder name in the model cache.
pub fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
        })
}

/// A SHA-256 written as 64 lower-case hex digits.
pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Adds `[ai]` and `[[models]]` to `project.toml`.
pub(crate) fn write(doc: &mut DocumentMut, ai: &AiSettings, models: &BTreeMap<String, ModelPin>) {
    let mut t = Table::new();
    if ai.commercial_safe_only {
        t.insert("commercial_safe_only", Item::Value(Value::from(true)));
    }
    if !ai.sovereign {
        t.insert("sovereign", Item::Value(Value::from(false)));
    }
    if ai.spend_cap_usd != 0.0 {
        t.insert("spend_cap_usd", Item::Value(Value::from(ai.spend_cap_usd)));
    }
    if !ai.hosts.is_empty() {
        let mut list = ArrayOfTables::new();
        for (name, addr) in &ai.hosts {
            let mut h = Table::new();
            h.insert("name", Item::Value(Value::from(name.as_str())));
            h.insert("addr", Item::Value(Value::from(addr.as_str())));
            list.push(h);
        }
        // Only hosts: `[[ai.hosts]]` alone, no empty `[ai]` header.
        t.set_implicit(t.is_empty());
        t.insert("hosts", Item::ArrayOfTables(list));
    }
    if !t.is_empty() {
        doc.insert("ai", Item::Table(t));
    }
    if !models.is_empty() {
        let mut list = ArrayOfTables::new();
        for (id, pin) in models {
            let mut t = Table::new();
            t.insert("id", Item::Value(Value::from(id.as_str())));
            t.insert(
                "licence_class",
                Item::Value(Value::from(pin.licence_class.as_str())),
            );
            t.insert("sha256", Item::Value(Value::from(pin.sha256.as_str())));
            list.push(t);
        }
        doc.insert("models", Item::ArrayOfTables(list));
    }
}

/// Reads `[ai]` and `[[models]]` from `project.toml`'s root table.
pub(crate) fn read(root: &Table) -> Result<(AiSettings, BTreeMap<String, ModelPin>), FormatError> {
    let mut ai = AiSettings::default();
    if let Some(item) = root.get("ai") {
        let t = table_like(item, "[ai]")?;
        only_keys(
            t,
            &[
                "commercial_safe_only",
                "sovereign",
                "spend_cap_usd",
                "hosts",
            ],
            "[ai]",
        )?;
        let flag = |key: &str, default: bool| match t.get(key) {
            None => Ok(default),
            Some(v) => v
                .as_bool()
                .ok_or_else(|| invalid(format!("[ai]: `{key}` must be true or false"))),
        };
        ai.commercial_safe_only = flag("commercial_safe_only", false)?;
        ai.sovereign = flag("sovereign", true)?;
        if let Some(v) = t.get("spend_cap_usd") {
            let usd = v
                .as_float()
                .or_else(|| v.as_integer().map(|i| i as f64))
                .filter(|f| f.is_finite() && *f >= 0.0)
                .ok_or_else(|| {
                    invalid("[ai]: `spend_cap_usd` must be a number of dollars, 0 or more".into())
                })?;
            ai.spend_cap_usd = usd;
        }
        for (i, h) in tables(t.get("hosts"), "ai.hosts")?.into_iter().enumerate() {
            let at = format!("[ai] host {}", i + 1);
            only_keys(h, &["name", "addr"], &at)?;
            let name = required_str(h, "name", &at)?;
            if !valid_host_name(name) {
                return Err(invalid(format!(
                    "{at}: `name` `{name}` must be lan:<name> or cloud:<name>, using a-z, 0-9, `-`, `_` and `.`"
                )));
            }
            let addr = required_str(h, "addr", &at)?;
            if !valid_host_addr(addr) {
                return Err(invalid(format!(
                    "{at} ({name}): `addr` `{addr}` must be host:port"
                )));
            }
            if ai.hosts.insert(name.to_owned(), addr.to_owned()).is_some() {
                return Err(invalid(format!("{at}: host `{name}` is listed twice")));
            }
        }
    }
    let mut models = BTreeMap::new();
    for (i, t) in tables(root.get("models"), "models")?
        .into_iter()
        .enumerate()
    {
        let at = format!("model {}", i + 1);
        only_keys(t, &["id", "licence_class", "sha256"], &at)?;
        let id = required_str(t, "id", &at)?;
        if !valid_model_id(id) {
            return Err(invalid(format!(
                "{at}: `id` `{id}` must use a-z, 0-9, `-`, `_` and `.`"
            )));
        }
        let class = required_str(t, "licence_class", &at)?;
        let licence_class = LicenceClass::parse(class).ok_or_else(|| {
            invalid(format!(
                "{at} ({id}): `licence_class` must be commercial-safe, restricted or non-commercial, not `{class}`"
            ))
        })?;
        let sha256 = required_str(t, "sha256", &at)?;
        if !valid_sha256(sha256) {
            return Err(invalid(format!(
                "{at} ({id}): `sha256` must be 64 lower-case hex digits"
            )));
        }
        let pin = ModelPin {
            licence_class,
            sha256: sha256.to_owned(),
        };
        if models.insert(id.to_owned(), pin).is_some() {
            return Err(invalid(format!("{at}: model `{id}` is listed twice")));
        }
    }
    Ok((ai, models))
}
