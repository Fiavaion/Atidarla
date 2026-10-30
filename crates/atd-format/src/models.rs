//! The models a project needs and its AI settings, in `project.toml`
//! (FR-A12, PRD A61).
//!
//! ```toml
//! [ai]
//! commercial_safe_only = true
//!
//! [[models]]
//! id = "sd-turbo"
//! licence_class = "restricted"
//! sha256 = "40ec4008…"          # the model's main file
//! ```
//!
//! Model files never live in a project: they are in the per-user model
//! cache (`atd_ai::models`). The project only names them, so opening it on
//! another machine can say which are missing. `[ai]` is written only when a
//! setting differs from its default and `[[models]]` only when there are
//! models, so projects without AI keep their bytes. Entries are written in
//! ID order.

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiSettings {
    /// Hide and refuse models whose licence class is not commercial-safe.
    pub commercial_safe_only: bool,
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
    if *ai != AiSettings::default() {
        let mut t = Table::new();
        t.insert(
            "commercial_safe_only",
            Item::Value(Value::from(ai.commercial_safe_only)),
        );
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
        only_keys(t, &["commercial_safe_only"], "[ai]")?;
        if let Some(v) = t.get("commercial_safe_only") {
            ai.commercial_safe_only = v.as_bool().ok_or_else(|| {
                invalid("[ai]: `commercial_safe_only` must be true or false".into())
            })?;
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
