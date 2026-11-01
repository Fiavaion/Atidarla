//! Licence acceptances (`accepted.toml`, per user, in the app's settings folder;
//! never in a project, PRD A61).
//!
//! ```toml
//! format = 1
//!
//! [[accepted]]
//! id = "sd-turbo"
//! licence = "LicenseRef-StabilityAI-Community"
//! url = "https://huggingface.co/stabilityai/sd-turbo/blob/<rev>/LICENSE.md"
//! date = "2026-10-30"
//! ```
//!
//! An acceptance counts only while the catalogue names the same licence ID
//! and URL: a model whose licence changes must be accepted again.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use super::catalogue::Model;

/// One accepted licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acceptance {
    pub licence: String,
    pub url: String,
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
}

/// Accepted licences by model ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Acceptances {
    pub by_id: BTreeMap<String, Acceptance>,
}

impl Acceptances {
    /// Reads `path`; a missing file means nothing was accepted.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes `path` (to a temporary file, then renamed).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let err = |e: std::io::Error| format!("{}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = path.with_extension("toml.tmp~");
        std::fs::write(&tmp, self.to_string()).map_err(err)?;
        std::fs::rename(&tmp, path).map_err(err)
    }

    /// Whether `model`'s current licence was accepted.
    pub fn is_accepted(&self, model: &Model) -> bool {
        self.by_id
            .get(&model.id)
            .is_some_and(|a| a.licence == model.licence.id && a.url == model.licence.url)
    }

    /// Records acceptance of `model`'s licence on `date`.
    pub fn accept(&mut self, model: &Model, date: impl Into<String>) {
        self.by_id.insert(
            model.id.clone(),
            Acceptance {
                licence: model.licence.id.clone(),
                url: model.licence.url.clone(),
                date: date.into(),
            },
        );
    }

    /// Withdraws acceptance. Returns whether there was one.
    pub fn revoke(&mut self, id: &str) -> bool {
        self.by_id.remove(id).is_some()
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Result<Self, String> {
        let doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| e.to_string())?;
        let root = doc.as_table();
        if let Some((k, _)) = root
            .iter()
            .find(|(k, _)| !["format", "accepted"].contains(k))
        {
            return Err(format!("unknown key `{k}`"));
        }
        if root.get("format").and_then(Item::as_integer) != Some(1) {
            return Err("`format` must be 1".into());
        }
        let mut out = Self::default();
        let Some(item) = root.get("accepted") else {
            return Ok(out);
        };
        let list = item
            .as_array_of_tables()
            .ok_or("`accepted` must be [[accepted]] tables")?;
        for (i, t) in list.iter().enumerate() {
            let get = |k: &str| {
                t.get(k)
                    .and_then(Item::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("entry {}: `{k}` must be a string", i + 1))
            };
            if let Some((k, _)) = t
                .iter()
                .find(|(k, _)| !["id", "licence", "url", "date"].contains(k))
            {
                return Err(format!("entry {}: unknown key `{k}`", i + 1));
            }
            let a = Acceptance {
                licence: get("licence")?,
                url: get("url")?,
                date: get("date")?,
            };
            out.by_id.insert(get("id")?, a);
        }
        Ok(out)
    }
}

impl std::fmt::Display for Acceptances {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut doc = DocumentMut::new();
        doc.insert("format", Item::Value(Value::from(1)));
        if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
            key.leaf_decor_mut().set_prefix(
                "# Model licences accepted on this machine (Lasair model manager).\n\
                 # Per user; never part of a project. See docs/models.md.\n",
            );
        }
        if !self.by_id.is_empty() {
            let mut list = ArrayOfTables::new();
            for (id, a) in &self.by_id {
                let mut t = Table::new();
                t.insert("id", Item::Value(Value::from(id.as_str())));
                t.insert("licence", Item::Value(Value::from(a.licence.as_str())));
                t.insert("url", Item::Value(Value::from(a.url.as_str())));
                t.insert("date", Item::Value(Value::from(a.date.as_str())));
                list.push(t);
            }
            doc.insert("accepted", Item::ArrayOfTables(list));
        }
        f.write_str(&doc.to_string())
    }
}

/// Today's date in UTC, `YYYY-MM-DD`.
pub fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    civil(secs / 86_400)
}

/// The proleptic Gregorian date of a day count since 1970-01-01
/// (H. Hinnant's `civil_from_days`).
fn civil(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_counts_become_dates() {
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(11_016), "2000-02-29");
        assert_eq!(civil(20_756), "2026-10-30");
        assert_eq!(today().len(), 10);
    }
}
