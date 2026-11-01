//! The model catalogue (`catalogue.toml`, embedded).

use std::sync::OnceLock;

use lsr_format::LicenceClass;
use lsr_format::models::{valid_model_id, valid_sha256};
use toml_edit::{DocumentMut, Item, TableLike};

const EMBEDDED: &str = include_str!("catalogue.toml");

/// A model's licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Licence {
    /// SPDX ID, or a `LicenseRef-…` for licences SPDX does not list.
    pub id: String,
    pub class: LicenceClass,
    /// Where to read it (pinned to the catalogue revision).
    pub url: String,
}

/// Where the files come from: a Hugging Face repository at one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub repo: String,
    /// A full 40-digit commit hash, never a branch.
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    /// Relative to the model's folder, `/`-separated.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// The AI Host adapter that runs it (`diffusion.sd`, `onnx`, …).
    pub adapter: String,
    pub task: String,
    /// Rough GPU memory needed, in MB.
    pub vram_mb: u32,
    /// Where it runs well (capability notes, PRD A19).
    pub platforms: String,
    pub licence: Licence,
    pub source: Source,
    pub files: Vec<ModelFile>,
    main: usize,
}

impl Model {
    /// The file a project manifest pins by SHA-256 (the main weights).
    pub fn main_file(&self) -> &ModelFile {
        &self.files[self.main]
    }

    /// Bytes to download.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The download URL of `file` under `base` (normally
    /// [`HUGGING_FACE`](super::HUGGING_FACE)).
    pub fn file_url(&self, base: &str, file: &ModelFile) -> String {
        format!(
            "{}/{}/resolve/{}/{}",
            base.trim_end_matches('/'),
            self.source.repo,
            self.source.revision,
            file.path
        )
    }
}

/// The models Lasair knows, in catalogue order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalogue {
    models: Vec<Model>,
}

impl Catalogue {
    pub fn get(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Model> {
        self.models.iter()
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

/// The embedded catalogue. Its validity is a unit test, so this cannot fail
/// in a tested build.
pub fn catalogue() -> &'static Catalogue {
    static CAT: OnceLock<Catalogue> = OnceLock::new();
    CAT.get_or_init(|| parse_catalogue(EMBEDDED).expect("embedded model catalogue is valid"))
}

fn str_at<'a>(t: &'a dyn TableLike, key: &str, at: &str) -> Result<&'a str, String> {
    t.get(key)
        .and_then(Item::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{at}: `{key}` must be a non-empty string"))
}

fn table<'a>(t: &'a dyn TableLike, key: &str, at: &str) -> Result<&'a dyn TableLike, String> {
    t.get(key)
        .and_then(Item::as_table_like)
        .ok_or_else(|| format!("{at}: `{key}` must be a table"))
}

fn only(t: &dyn TableLike, keys: &[&str], at: &str) -> Result<(), String> {
    match t.iter().find(|(k, _)| !keys.contains(k)) {
        Some((k, _)) => Err(format!("{at}: unknown key `{k}`")),
        None => Ok(()),
    }
}

/// A relative path inside a model folder: no `..`, no root, no `\`.
fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.ends_with(".part"))
}

fn is_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parses and checks a catalogue: unique IDs, a known licence class, a
/// pinned revision, safe file paths, a SHA-256 for every file.
pub fn parse_catalogue(text: &str) -> Result<Catalogue, String> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let root = doc.as_table();
    only(root, &["format", "model"], "catalogue")?;
    if root.get("format").and_then(Item::as_integer) != Some(1) {
        return Err("catalogue: `format` must be 1".into());
    }
    let list = root
        .get("model")
        .and_then(Item::as_array_of_tables)
        .ok_or("catalogue: no [[model]] entries")?;
    let mut models: Vec<Model> = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let at = format!("model {}", i + 1);
        only(
            t,
            &[
                "id",
                "name",
                "adapter",
                "task",
                "vram_mb",
                "platforms",
                "main",
                "licence",
                "source",
                "files",
            ],
            &at,
        )?;
        let id = str_at(t, "id", &at)?;
        let at = format!("{at} ({id})");
        if !valid_model_id(id) {
            return Err(format!("{at}: `id` must use a-z, 0-9, `-`, `_` and `.`"));
        }
        if models.iter().any(|m| m.id == id) {
            return Err(format!("{at}: listed twice"));
        }
        let lic = table(t, "licence", &at)?;
        only(lic, &["id", "class", "url"], &at)?;
        let class = str_at(lic, "class", &at)?;
        let licence = Licence {
            id: str_at(lic, "id", &at)?.to_owned(),
            class: LicenceClass::parse(class)
                .ok_or_else(|| format!("{at}: unknown licence class `{class}`"))?,
            url: str_at(lic, "url", &at)?.to_owned(),
        };
        if !licence.url.starts_with("https://") {
            return Err(format!("{at}: the licence URL must be https"));
        }
        let src = table(t, "source", &at)?;
        only(src, &["repo", "revision"], &at)?;
        let source = Source {
            repo: str_at(src, "repo", &at)?.to_owned(),
            revision: str_at(src, "revision", &at)?.to_owned(),
        };
        if !is_commit(&source.revision) {
            return Err(format!(
                "{at}: `revision` must be a 40-digit commit hash, not a branch"
            ));
        }
        if source.repo.split('/').count() != 2 || !safe_path(&source.repo) {
            return Err(format!("{at}: `repo` must be owner/name"));
        }
        let arr = t
            .get("files")
            .and_then(Item::as_array)
            .ok_or_else(|| format!("{at}: `files` must be a list"))?;
        let mut files: Vec<ModelFile> = Vec::new();
        for v in arr {
            let f = v
                .as_inline_table()
                .ok_or_else(|| format!("{at}: `files` entries must be tables"))?;
            only(f, &["path", "size", "sha256"], &at)?;
            let path = str_at(f, "path", &at)?;
            if !safe_path(path) {
                return Err(format!("{at}: unsafe file path `{path}`"));
            }
            if files.iter().any(|x| x.path == path) {
                return Err(format!("{at}: `{path}` listed twice"));
            }
            let size = TableLike::get(f, "size")
                .and_then(Item::as_integer)
                .and_then(|s| u64::try_from(s).ok())
                .filter(|s| *s > 0)
                .ok_or_else(|| format!("{at}: `{path}` needs a size > 0"))?;
            let sha256 = str_at(f, "sha256", &at)?;
            if !valid_sha256(sha256) {
                return Err(format!("{at}: `{path}` needs a lower-case hex SHA-256"));
            }
            files.push(ModelFile {
                path: path.to_owned(),
                size,
                sha256: sha256.to_owned(),
            });
        }
        let main_path = str_at(t, "main", &at)?;
        let main = files
            .iter()
            .position(|f| f.path == main_path)
            .ok_or_else(|| format!("{at}: `main` `{main_path}` is not in `files`"))?;
        let vram_mb = t
            .get("vram_mb")
            .and_then(Item::as_integer)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("{at}: `vram_mb` must be a whole number"))?;
        models.push(Model {
            id: id.to_owned(),
            name: str_at(t, "name", &at)?.to_owned(),
            adapter: str_at(t, "adapter", &at)?.to_owned(),
            task: str_at(t, "task", &at)?.to_owned(),
            vram_mb,
            platforms: str_at(t, "platforms", &at)?.to_owned(),
            licence,
            source,
            files,
            main,
        });
    }
    Ok(Catalogue { models })
}
