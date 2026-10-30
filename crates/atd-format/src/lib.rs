//! Project folders (`*.atidarla/`) as text files (PRD §5.5, A11, FR-F1).
//!
//! ```text
//! myshow.atidarla/
//!   project.toml          name, description, output resolution
//!   networks/main.toml    the patch: nodes, inputs, parameters
//!   packs/<name>/         the project's copy of each Pack it uses (see [`pack`])
//!   mappings.toml         MIDI/OSC mappings and OSC settings (see [`mappings`])
//!   timeline.toml         keyframe tracks, markers and cues (optional, see [`timeline`])
//!   media/ shaders/ ...   files the patch refers to by relative path
//!   .cache/               binaries, ignored by Git
//! ```
//!
//! Saving the same project always writes the same bytes, and loading what
//! was saved gives the same project back (CLAUDE.md rule 8). Files are
//! written to a temporary name and renamed, so a crash mid-save leaves the
//! previous version intact. Loading checks structure only; check networks
//! against a registry with [`atd_core::graph::Network::validated`] (or
//! `Engine::load`) before use.
//!
//! Path parameters (movies, audio, fonts) may be relative: they are
//! relative to the project folder (`Params::resolve` in `atd-gpu`).

pub mod mappings;
mod network;
pub mod pack;
pub mod timeline;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use atd_core::graph::Network;
use atd_core::pack::PackSet;
use toml_edit::{Array, DocumentMut, Item, Value};

pub use mappings::{Mappings, mappings_from_str, mappings_to_string};
pub use network::{network_from_str, network_to_string};
pub use pack::{Library, load_pack, pack_from_str, pack_to_string, save_pack};
pub use timeline::{TimelineDoc, timeline_from_str, timeline_to_string};

/// The file format version this build reads and writes.
pub const FORMAT: i64 = 1;

/// The network a project opens with.
pub const MAIN: &str = "main";

/// Folder name suffix of a project.
pub const EXTENSION: &str = "atidarla";

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("{file}: {message}")]
    InFile { file: String, message: String },
}

impl FormatError {
    fn in_file(self, file: &str) -> Self {
        match self {
            Self::Invalid(message) => Self::InFile {
                file: file.to_owned(),
                message,
            },
            other => other,
        }
    }
}

/// A project: settings and its networks by ID.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Display name.
    pub name: String,
    /// What the project is and how to use it (shown when opened).
    pub description: String,
    /// Output resolution in pixels.
    pub resolution: [u32; 2],
    /// Networks by ID (file stem in `networks/`). [`MAIN`] is the patch.
    pub networks: BTreeMap<String, Network>,
    /// The project's Pack definitions (`packs/<name>/`). Check them with
    /// [`PackSet::register`] before use.
    pub packs: PackSet,
    /// MIDI/OSC mappings (`mappings.toml`; none when the file is absent).
    pub mappings: Mappings,
    /// The timeline (`timeline.toml`); `None` when the project has none.
    pub timeline: Option<TimelineDoc>,
}

impl Project {
    /// An empty project with an empty main network.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            resolution: [1280, 720],
            networks: BTreeMap::from([(MAIN.to_owned(), Network::default())]),
            packs: PackSet::default(),
            mappings: Mappings::default(),
            timeline: None,
        }
    }

    /// A project whose main network is `net`.
    pub fn with_main(name: impl Into<String>, net: Network) -> Self {
        let mut p = Self::new(name);
        p.networks.insert(MAIN.to_owned(), net);
        p
    }

    pub fn main(&self) -> Option<&Network> {
        self.networks.get(MAIN)
    }
}

/// A network ID: lower-case letters, digits, `_` and `-`, so it is a
/// portable file name.
fn valid_network_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// `project.toml` as text.
pub fn project_to_string(p: &Project) -> String {
    let mut doc = DocumentMut::new();
    doc.insert("format", Item::Value(Value::from(FORMAT)));
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut()
            .set_prefix("# Atidarla project. The patch is in networks/main.toml.\n");
    }
    doc.insert("name", Item::Value(network::string(&p.name)));
    if !p.description.is_empty() {
        doc.insert("description", Item::Value(network::string(&p.description)));
    }
    let mut res = Array::new();
    res.push(i64::from(p.resolution[0]));
    res.push(i64::from(p.resolution[1]));
    doc.insert("resolution", Item::Value(Value::Array(res)));
    doc.to_string()
}

/// Parses `project.toml`; the returned project has no networks.
pub fn project_from_str(source: &str) -> Result<Project, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| FormatError::Invalid(e.to_string().trim_end().into()))?;
    let root = doc.as_table();
    network::only_keys(
        root,
        &["format", "name", "description", "resolution"],
        "the file",
    )?;
    network::check_format(root)?;
    let name = network::required_str(root, "name", "the file")?.to_owned();
    let description = match root.get("description") {
        Some(_) => network::required_str(root, "description", "the file")?.to_owned(),
        None => String::new(),
    };
    let resolution = match root.get("resolution") {
        None => [1280, 720],
        Some(item) => {
            let bad = || {
                FormatError::Invalid("`resolution` must be [width, height], each 1 to 16384".into())
            };
            let a = item.as_array().ok_or_else(bad)?;
            let v: Vec<u32> = a
                .iter()
                .map(|v| v.as_integer().and_then(|i| u32::try_from(i).ok()))
                .collect::<Option<_>>()
                .filter(|v: &Vec<u32>| v.len() == 2 && v.iter().all(|x| (1..=16_384).contains(x)))
                .ok_or_else(bad)?;
            [v[0], v[1]]
        }
    };
    Ok(Project {
        name,
        description,
        resolution,
        networks: BTreeMap::new(),
        packs: PackSet::default(),
        mappings: Mappings::default(),
        timeline: None,
    })
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> FormatError + '_ {
    move |source| FormatError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Reads a project folder.
pub fn load(dir: &Path) -> Result<Project, FormatError> {
    let path = dir.join("project.toml");
    let text = std::fs::read_to_string(&path).map_err(io(&path))?;
    let mut project = project_from_str(&text).map_err(|e| e.in_file("project.toml"))?;
    let networks = dir.join("networks");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&networks)
        .map_err(io(&networks))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()
        .map_err(io(&networks))?;
    files.retain(|p| p.extension().is_some_and(|e| e == "toml"));
    files.sort();
    for path in files {
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let file = format!("networks/{id}.toml");
        if !valid_network_id(&id) {
            return Err(FormatError::InFile {
                file,
                message: "network file names use a-z, 0-9, `_` and `-`".into(),
            });
        }
        let text = std::fs::read_to_string(&path).map_err(io(&path))?;
        let net = network_from_str(&text).map_err(|e| e.in_file(&file))?;
        project.networks.insert(id, net);
    }
    if !project.networks.contains_key(MAIN) {
        return Err(FormatError::Invalid(format!(
            "{}: no networks/{MAIN}.toml",
            dir.display()
        )));
    }
    let path = dir.join(mappings::FILE);
    if path.exists() {
        let text = std::fs::read_to_string(&path).map_err(io(&path))?;
        project.mappings = mappings_from_str(&text).map_err(|e| e.in_file(mappings::FILE))?;
    }
    // Folders without a pack.toml (left-overs, the user's own) are skipped.
    for path in pack::subdirs(&dir.join("packs"))? {
        if !path.join("pack.toml").exists() {
            continue;
        }
        let folder = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let rel = format!("packs/{folder}");
        let def = load_pack(&path, &rel)?;
        if def.name != folder {
            return Err(FormatError::InFile {
                file: format!("{rel}/pack.toml"),
                message: format!(
                    "Pack `{}` must be in a folder named `{}`",
                    def.name, def.name
                ),
            });
        }
        project.packs.insert(def);
    }
    let path = dir.join(timeline::FILE);
    if path.exists() {
        let text = std::fs::read_to_string(&path).map_err(io(&path))?;
        project.timeline = Some(timeline_from_str(&text).map_err(|e| e.in_file(timeline::FILE))?);
    }
    Ok(project)
}

/// Writes a project folder, creating it if needed. Network files no longer
/// in the project are removed, and `timeline.toml` when the project has no
/// timeline; other files in the folder are left alone.
pub fn save(dir: &Path, project: &Project) -> Result<(), FormatError> {
    for id in project.networks.keys() {
        if !valid_network_id(id) {
            return Err(FormatError::Invalid(format!(
                "network ID `{id}`: use a-z, 0-9, `_` and `-`"
            )));
        }
    }
    let mut files = vec![(PathBuf::from("project.toml"), project_to_string(project))];
    for (id, net) in &project.networks {
        let text = network_to_string(net).map_err(|e| e.in_file(&format!("networks/{id}.toml")))?;
        files.push((Path::new("networks").join(format!("{id}.toml")), text));
    }
    let mappings_path = dir.join(mappings::FILE);
    let write_mappings = !project.mappings.is_empty();
    if write_mappings {
        let text = mappings_to_string(&project.mappings).map_err(|e| e.in_file(mappings::FILE))?;
        files.push((PathBuf::from(mappings::FILE), text));
    }
    if let Some(tl) = &project.timeline {
        let text = timeline_to_string(tl).map_err(|e| e.in_file(timeline::FILE))?;
        files.push((PathBuf::from(timeline::FILE), text));
    }
    let networks = dir.join("networks");
    std::fs::create_dir_all(&networks).map_err(io(&networks))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        write_atomic(&ignore, ".cache/\n")?;
    }
    for (rel, text) in &files {
        write_atomic(&dir.join(rel), text)?;
    }
    for entry in std::fs::read_dir(&networks).map_err(io(&networks))? {
        let path = entry.map_err(io(&networks))?.path();
        let stale = path.extension().is_some_and(|e| e == "toml")
            && path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|id| !project.networks.contains_key(id));
        if stale {
            std::fs::remove_file(&path).map_err(io(&path))?;
        }
    }
    // No mappings left: the file goes, so the folder matches the project.
    if !write_mappings && mappings_path.exists() {
        std::fs::remove_file(&mappings_path).map_err(io(&mappings_path))?;
    }
    let tl = dir.join(timeline::FILE);
    if project.timeline.is_none() && tl.exists() {
        std::fs::remove_file(&tl).map_err(io(&tl))?;
    }
    let packs = dir.join("packs");
    for def in project.packs.iter() {
        pack::save_pack(&packs.join(&def.name), def)
            .map_err(|e| e.in_file(&format!("packs/{}", def.name)))?;
    }
    // A Pack no longer in the project: its files go, other files stay.
    for path in pack::subdirs(&packs)? {
        let stale = path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| project.packs.get(name).is_none());
        if stale {
            pack::remove_pack(&path)?;
        }
    }
    Ok(())
}

/// Writes `text` to a temporary file beside `path`, then renames it over
/// `path`, so readers see the old or the new file, never half of one.
fn write_atomic(path: &Path, text: &str) -> Result<(), FormatError> {
    let tmp = path.with_extension("tmp~");
    std::fs::write(&tmp, text).map_err(io(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io(path))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pack_tests;

#[cfg(test)]
mod mappings_tests;

#[cfg(test)]
mod timeline_tests;
