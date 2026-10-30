//! The Stage media bin's index (FR-B3): `media/bin/index.toml`, one entry
//! per batch work item whose image is `media/bin/<hash>.png`.
//!
//! ```toml
//! format = 1
//!
//! [[item]]
//! hash = "3f0c…"                # SHA-256 cache key of the item (64 hex digits)
//! wedge = "seeds"
//! index = 12                    # the item's position in its wedge
//! host = "lan:studio"           # where it cooked
//! size = [512, 512]
//! created = 2026-10-30T12:00:00Z
//! cook_ms = 812.5
//! attributes = { prompt = "a red fox", "diffusion1.seed" = 3 }
//! ```
//!
//! The hash is the item's cache key (its upstream network, parameter
//! values, frame and size), so one image may be listed under several
//! wedges, but at most once per wedge. Entries are written sorted by
//! wedge, index and hash, attributes by name, so the file is stable text
//! (CLAUDE.md rule 8). A project without the file has an empty bin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use toml_edit::{
    Array, ArrayOfTables, Date, Datetime, DocumentMut, InlineTable, Item, Offset, Table, Time,
    Value,
};

use crate::batch::valid_wedge_name;
use crate::models::valid_sha256;
use crate::network::{
    check_format, invalid, json_to_toml, only_keys, required_str, required_u64, string, tables,
    toml_to_json,
};
use crate::{FORMAT, FormatError};

/// The bin's folder inside the project folder.
pub const DIR: &str = "media/bin";

/// The index file inside the project folder.
pub const INDEX: &str = "media/bin/index.toml";

const HEADER: &str =
    "# Atidarla media bin: batch results, images in media/bin/<hash>.png (docs/batch.md).\n";

/// One work item's result.
#[derive(Debug, Clone, PartialEq)]
pub struct BinEntry {
    /// The item's cache key, lower-case hex; the image is `<hash>.png`.
    pub hash: String,
    pub wedge: String,
    /// Position of the item in its wedge.
    pub index: u64,
    /// The host it cooked on (`local`, `lan:<x>`, `cloud:<x>`, `comfy:<addr>`).
    pub host: String,
    pub size: [u32; 2],
    /// When it was made, in seconds since 1970-01-01 UTC.
    pub created: u64,
    /// How long the item took to cook, in milliseconds.
    pub cook_ms: f64,
    /// The values the wedge gave it, by attribute name.
    pub attributes: BTreeMap<String, Json>,
}

impl BinEntry {
    /// The image's path relative to the project folder.
    pub fn image(&self) -> String {
        image_rel(&self.hash)
    }
}

/// `media/bin/<hash>.png`.
pub fn image_rel(hash: &str) -> String {
    format!("{DIR}/{hash}.png")
}

/// The media bin's entries, sorted by wedge, index and hash.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaBin {
    entries: Vec<BinEntry>,
}

impl MediaBin {
    pub fn entries(&self) -> &[BinEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `wedge` lists `hash`.
    pub fn contains(&self, wedge: &str, hash: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.wedge == wedge && e.hash == hash)
    }

    /// Any entry for `hash` (whichever wedge made it).
    pub fn find(&self, hash: &str) -> Option<&BinEntry> {
        self.entries.iter().find(|e| e.hash == hash)
    }

    /// Adds an entry, replacing the one with the same wedge and hash.
    pub fn insert(&mut self, entry: BinEntry) {
        self.entries
            .retain(|e| !(e.wedge == entry.wedge && e.hash == entry.hash));
        let at = self.entries.partition_point(|e| key(e) < key(&entry));
        self.entries.insert(at, entry);
    }

    /// Removes the entries `keep` refuses; returns how many went.
    pub fn retain(&mut self, keep: impl FnMut(&BinEntry) -> bool) -> usize {
        let before = self.entries.len();
        self.entries.retain(keep);
        before - self.entries.len()
    }
}

fn key(e: &BinEntry) -> (&str, u64, &str) {
    (&e.wedge, e.index, &e.hash)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number from [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `created` as people read it: `2026-10-30T12:00:00Z`.
pub fn created_text(secs: u64) -> String {
    datetime(secs).to_string()
}

/// Seconds since 1970 as a UTC TOML datetime.
fn datetime(secs: u64) -> Datetime {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    Datetime {
        date: Some(Date {
            year: y as u16,
            month: m as u8,
            day: d as u8,
        }),
        time: Some(Time {
            hour: (rem / 3600) as u8,
            minute: (rem % 3600 / 60) as u8,
            second: Some((rem % 60) as u8),
            nanosecond: None,
        }),
        offset: Some(Offset::Z),
    }
}

/// A TOML datetime with a date, time and offset as seconds since 1970 UTC.
fn seconds(dt: &Datetime) -> Option<u64> {
    let date = dt.date?;
    let time = dt.time?;
    let offset_min = match dt.offset? {
        Offset::Z => 0,
        Offset::Custom { minutes } => i64::from(minutes),
    };
    let days = days_from_civil(
        i64::from(date.year),
        u32::from(date.month),
        u32::from(date.day),
    );
    let secs = days * 86_400
        + i64::from(time.hour) * 3600
        + i64::from(time.minute) * 60
        + i64::from(time.second.unwrap_or(0))
        - offset_min * 60;
    u64::try_from(secs).ok()
}

fn int(v: u64) -> Result<Value, FormatError> {
    i64::try_from(v)
        .map(Value::from)
        .map_err(|_| invalid(format!("{v} is too large")))
}

/// The index as text.
pub fn bin_to_string(bin: &MediaBin) -> Result<String, FormatError> {
    let mut doc = DocumentMut::new();
    doc.insert("format", Item::Value(Value::from(FORMAT)));
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(HEADER);
    }
    let mut list = ArrayOfTables::new();
    for e in &bin.entries {
        let mut t = Table::new();
        t.insert("hash", Item::Value(Value::from(e.hash.as_str())));
        t.insert("wedge", Item::Value(Value::from(e.wedge.as_str())));
        t.insert("index", Item::Value(int(e.index)?));
        t.insert("host", Item::Value(string(&e.host)));
        let mut size = Array::new();
        size.push(i64::from(e.size[0]));
        size.push(i64::from(e.size[1]));
        t.insert("size", Item::Value(Value::Array(size)));
        t.insert("created", Item::Value(Value::from(datetime(e.created))));
        t.insert("cook_ms", Item::Value(Value::from(e.cook_ms)));
        let mut attrs = InlineTable::new();
        for (k, v) in &e.attributes {
            attrs.insert(
                k,
                json_to_toml(v).map_err(|m| invalid(format!("item {}: {k}: {m}", e.hash)))?,
            );
        }
        t.insert("attributes", Item::Value(Value::InlineTable(attrs)));
        list.push(t);
    }
    if !list.is_empty() {
        doc.insert("item", Item::ArrayOfTables(list));
    }
    Ok(doc.to_string())
}

/// Parses the index.
pub fn bin_from_str(source: &str) -> Result<MediaBin, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(e.to_string().trim_end().to_owned()))?;
    let root = doc.as_table();
    only_keys(root, &["format", "item"], "the file")?;
    check_format(root)?;
    let mut bin = MediaBin::default();
    for (i, t) in tables(root.get("item"), "item")?.into_iter().enumerate() {
        let at = format!("item {}", i + 1);
        only_keys(
            t,
            &[
                "hash",
                "wedge",
                "index",
                "host",
                "size",
                "created",
                "cook_ms",
                "attributes",
            ],
            &at,
        )?;
        let hash = required_str(t, "hash", &at)?;
        if !valid_sha256(hash) {
            return Err(invalid(format!(
                "{at}: `hash` must be 64 lower-case hex digits"
            )));
        }
        let wedge = required_str(t, "wedge", &at)?;
        if !valid_wedge_name(wedge) {
            return Err(invalid(format!(
                "{at}: `wedge` `{wedge}` must use a-z, 0-9, `_` and `-`"
            )));
        }
        let size = t
            .get("size")
            .and_then(Item::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_integer().and_then(|i| u32::try_from(i).ok()))
                    .collect::<Option<Vec<u32>>>()
            })
            .and_then(|v| v.filter(|v| v.len() == 2))
            .ok_or_else(|| invalid(format!("{at}: `size` must be [width, height]")))?;
        let created = t
            .get("created")
            .and_then(Item::as_datetime)
            .and_then(seconds)
            .ok_or_else(|| {
                invalid(format!(
                    "{at}: `created` must be a date and time with an offset, like 2026-10-30T12:00:00Z"
                ))
            })?;
        let cook_ms = t
            .get("cook_ms")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .filter(|f| f.is_finite() && *f >= 0.0)
            .ok_or_else(|| invalid(format!("{at}: `cook_ms` must be a number, 0 or more")))?;
        let mut attributes = BTreeMap::new();
        if let Some(a) = t.get("attributes") {
            let a = a
                .as_table_like()
                .ok_or_else(|| invalid(format!("{at}: `attributes` must be a table")))?;
            for (k, v) in a.iter() {
                let v = v
                    .as_value()
                    .ok_or_else(|| invalid(format!("{at}: attribute `{k}` must be a value")))?;
                attributes.insert(
                    k.to_owned(),
                    toml_to_json(v).map_err(|e| invalid(format!("{at}: attribute `{k}`: {e}")))?,
                );
            }
        }
        let entry = BinEntry {
            hash: hash.to_owned(),
            wedge: wedge.to_owned(),
            index: required_u64(t, "index", &at)?,
            host: required_str(t, "host", &at)?.to_owned(),
            size: [size[0], size[1]],
            created,
            cook_ms,
            attributes,
        };
        if bin.contains(&entry.wedge, &entry.hash) {
            return Err(invalid(format!(
                "{at}: `{}` is listed twice for wedge `{}`",
                entry.hash, entry.wedge
            )));
        }
        bin.insert(entry);
    }
    Ok(bin)
}

/// The bin folder of the project at `dir`.
pub fn bin_dir(dir: &Path) -> PathBuf {
    dir.join(DIR)
}

/// Reads the project's media bin index; empty when there is none.
pub fn load_bin(dir: &Path) -> Result<MediaBin, FormatError> {
    let path = dir.join(INDEX);
    if !path.exists() {
        return Ok(MediaBin::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|source| FormatError::Io {
        path: path.clone(),
        source,
    })?;
    bin_from_str(&text).map_err(|e| match e {
        FormatError::Invalid(message) => FormatError::InFile {
            file: INDEX.to_owned(),
            message,
        },
        other => other,
    })
}

/// Writes the project's media bin index (creating the folder).
pub fn save_bin(dir: &Path, bin: &MediaBin) -> Result<(), FormatError> {
    let folder = bin_dir(dir);
    std::fs::create_dir_all(&folder).map_err(|source| FormatError::Io {
        path: folder.clone(),
        source,
    })?;
    crate::write_atomic(&dir.join(INDEX), &bin_to_string(bin)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const H1: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
    const H2: &str = "aa1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

    fn entry(wedge: &str, index: u64, hash: &str) -> BinEntry {
        BinEntry {
            hash: hash.into(),
            wedge: wedge.into(),
            index,
            host: "lan:a".into(),
            size: [64, 32],
            created: 1_761_825_600,
            cook_ms: 12.5,
            attributes: BTreeMap::from([
                ("prompt".to_owned(), json!("a red fox")),
                ("diffusion1.seed".to_owned(), json!(3)),
                ("colour".to_owned(), json!([0.5, 0.25, 1.0, 1.0])),
            ]),
        }
    }

    #[test]
    fn the_index_round_trips_sorted_and_byte_stable() {
        let mut bin = MediaBin::default();
        bin.insert(entry("seeds", 2, H2));
        bin.insert(entry("prompts", 0, H2));
        bin.insert(entry("seeds", 0, H1));
        let text = bin_to_string(&bin).unwrap();
        let back = bin_from_str(&text).unwrap();
        assert_eq!(back, bin);
        assert_eq!(bin_to_string(&back).unwrap(), text, "byte-stable");
        let order: Vec<(&str, u64)> = back
            .entries()
            .iter()
            .map(|e| (e.wedge.as_str(), e.index))
            .collect();
        assert_eq!(order, [("prompts", 0), ("seeds", 0), ("seeds", 2)]);
        assert!(text.contains("created = 2025-10-30T12:00:00Z"), "{text}");
        assert!(text.contains("\"diffusion1.seed\" = 3"), "{text}");
        // Attributes in name order.
        let c = text.find("colour =").unwrap();
        let p = text.find("prompt =").unwrap();
        assert!(c < p, "{text}");
        assert_eq!(
            bin_to_string(&MediaBin::default()).unwrap().lines().count(),
            2
        );
    }

    #[test]
    fn one_entry_per_wedge_and_hash() {
        let mut bin = MediaBin::default();
        bin.insert(entry("seeds", 0, H1));
        let mut again = entry("seeds", 0, H1);
        again.host = "lan:b".into();
        bin.insert(again);
        assert_eq!(bin.len(), 1);
        assert_eq!(bin.entries()[0].host, "lan:b");
        bin.insert(entry("other", 0, H1));
        assert_eq!(bin.len(), 2);
        assert!(bin.contains("other", H1) && bin.find(H1).is_some());
        let text = bin_to_string(&bin)
            .unwrap()
            .replace("\"other\"", "\"seeds\"");
        let err = bin_from_str(&text).unwrap_err().to_string();
        assert!(err.contains("listed twice"), "{err}");
    }

    #[test]
    fn dates_convert_both_ways_with_offsets() {
        for secs in [0, 951_782_400, 1_761_825_600, 4_102_444_799] {
            assert_eq!(seconds(&datetime(secs)), Some(secs));
        }
        assert_eq!(datetime(951_782_400).to_string(), "2000-02-29T00:00:00Z");
        let dt: Datetime = "2026-10-30T14:00:00+02:00".parse().unwrap();
        assert_eq!(
            datetime(seconds(&dt).unwrap()).to_string(),
            "2026-10-30T12:00:00Z"
        );
    }

    #[test]
    fn bad_entries_are_refused_with_the_item() {
        for (text, want) in [
            ("format = 1\n[[item]]\nhash = \"x\"", "64 lower-case hex"),
            ("format = 1\nextra = 1", "unknown key `extra`"),
            ("format = 2", "newer Atidarla"),
        ] {
            let err = bin_from_str(text).unwrap_err().to_string();
            assert!(err.contains(want), "{text}: {err}");
        }
        let good = bin_to_string(&{
            let mut b = MediaBin::default();
            b.insert(entry("seeds", 0, H1));
            b
        })
        .unwrap();
        let bad = good.replace("created = 2025-10-30T12:00:00Z", "created = 2025-10-30");
        let err = bin_from_str(&bad).unwrap_err().to_string();
        assert!(err.contains("item 1: `created`"), "{err}");
    }
}
