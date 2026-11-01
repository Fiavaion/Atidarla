//! Pack folders (PRD §5.5, FR-P4, `docs/packs.md`).
//!
//! ```text
//! packs/glow/
//!   pack.toml      name, version, help summary, lock, promoted parameters, sockets
//!   network.toml   the network inside, in the same form as networks/*.toml
//!   help.md        the embedded help page (written when there is help)
//! ```
//!
//! The same folder is used inside a project (`<project>/packs/<name>/`,
//! the project's own copy of each Pack it uses) and in a Pack library such
//! as the repository's `packs/` (`packs/<name>/` or, for an older version
//! kept beside the current one, `packs/<name>@<version>/`).
//!
//! `pack.toml`:
//!
//! ```toml
//! # Lasair Pack. Its network is network.toml; its help page is help.md.
//! format = 1
//! name = "glow"
//! version = "1.0.0"
//! label = "Glow"
//! summary = "A soft glow around bright areas."
//! locked = true
//!
//! [[param]]
//! name = "amount"
//! label = "Amount"
//! default = 0.8
//! min = 0.0
//! max = 2.0
//! help = "How strong the glow is."
//! to = [{ node = 1, param = "intensity" }]
//!
//! [[input]]
//! name = "in"
//! to = [{ node = 0, input = "in" }]
//!
//! [[output]]
//! name = "out"
//! from = { node = 2, output = "out" }
//! ```
//!
//! Keys are written in this order, empty optional keys are left out, and
//! node references are stable IDs into `network.toml`, so changing a
//! default or a range is a one-line diff. Unknown keys are errors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lsr_core::graph::{Endpoint, NodeId};
use lsr_core::pack::{
    PackDef, PackInput, PackOutput, PackRef, ParamTarget, PromotedParam, Version, VersionReq,
    valid_pack_name,
};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::network::{
    self, check_format, invalid, json_to_toml, only_keys, required_str, required_u64, table_like,
    tables, toml_to_json,
};
use crate::{FORMAT, FormatError, io, network_from_str, network_to_string, write_atomic};

const HEADER: &str = "# Lasair Pack. Its network is network.toml; its help page is help.md.\n";

/// The files of one Pack folder, by name.
const FILES: [&str; 3] = ["pack.toml", "network.toml", "help.md"];

/// `pack.toml` as text. The same definition always gives the same text.
pub fn pack_to_string(def: &PackDef) -> Result<String, FormatError> {
    let mut doc = DocumentMut::new();
    let mut format = Item::Value(Value::from(FORMAT));
    if let Some(v) = format.as_value_mut() {
        v.decor_mut().set_prefix(" ");
    }
    doc.insert("format", format);
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(HEADER);
    }
    doc.insert("name", Item::Value(network::string(&def.name)));
    doc.insert("version", Item::Value(Value::from(def.version.to_string())));
    if !def.label.is_empty() {
        doc.insert("label", Item::Value(network::string(&def.label)));
    }
    if !def.summary.is_empty() {
        doc.insert("summary", Item::Value(network::string(&def.summary)));
    }
    doc.insert("locked", Item::Value(Value::from(def.locked)));
    if let Some(r) = &def.derived_from {
        doc.insert("derived_from", Item::Value(Value::from(r.to_string())));
    }

    let id = |n: NodeId| {
        i64::try_from(n.0)
            .map(Value::from)
            .map_err(|_| invalid(format!("node ID {} is too large", n.0)))
    };
    let mut params = ArrayOfTables::new();
    for p in &def.params {
        let at = format!("parameter `{}`", p.name);
        let mut t = Table::new();
        t.insert("name", Item::Value(network::string(&p.name)));
        if !p.label.is_empty() {
            t.insert("label", Item::Value(network::string(&p.label)));
        }
        let default = json_to_toml(&p.default).map_err(|e| invalid(format!("{at}: {e}")))?;
        t.insert("default", Item::Value(default));
        for (key, v) in [("min", p.min), ("max", p.max)] {
            if let Some(v) = v {
                t.insert(key, Item::Value(Value::from(v)));
            }
        }
        if !p.help.is_empty() {
            t.insert("help", Item::Value(network::string(&p.help)));
        }
        let mut to = Array::new();
        for target in &p.targets {
            let mut it = InlineTable::new();
            it.insert("node", id(target.node)?);
            it.insert("param", network::string(&target.param));
            to.push(Value::InlineTable(it));
        }
        t.insert("to", Item::Value(Value::Array(to)));
        params.push(t);
    }
    if !params.is_empty() {
        doc.insert("param", Item::ArrayOfTables(params));
    }

    let mut inputs = ArrayOfTables::new();
    for i in &def.inputs {
        let mut t = Table::new();
        t.insert("name", Item::Value(network::string(&i.name)));
        let mut to = Array::new();
        for e in &i.targets {
            let mut it = InlineTable::new();
            it.insert("node", id(e.node)?);
            it.insert("input", network::string(&e.socket));
            to.push(Value::InlineTable(it));
        }
        t.insert("to", Item::Value(Value::Array(to)));
        inputs.push(t);
    }
    if !inputs.is_empty() {
        doc.insert("input", Item::ArrayOfTables(inputs));
    }

    let mut outputs = ArrayOfTables::new();
    for o in &def.outputs {
        let mut t = Table::new();
        t.insert("name", Item::Value(network::string(&o.name)));
        let mut from = InlineTable::new();
        from.insert("node", id(o.source.node)?);
        from.insert("output", network::string(&o.source.socket));
        t.insert("from", Item::Value(Value::InlineTable(from)));
        outputs.push(t);
    }
    if !outputs.is_empty() {
        doc.insert("output", Item::ArrayOfTables(outputs));
    }
    Ok(doc.to_string())
}

/// Parses `pack.toml`; the definition has an empty network and no help
/// (they live in `network.toml` and `help.md`).
pub fn pack_from_str(source: &str) -> Result<PackDef, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(e.to_string().trim_end().to_owned()))?;
    let root = doc.as_table();
    only_keys(
        root,
        &[
            "format",
            "name",
            "version",
            "label",
            "summary",
            "locked",
            "derived_from",
            "param",
            "input",
            "output",
        ],
        "the file",
    )?;
    check_format(root)?;
    let name = required_str(root, "name", "the file")?.to_owned();
    if !valid_pack_name(&name) {
        return Err(invalid(
            lsr_core::pack::PackError::InvalidName(name).to_string(),
        ));
    }
    let version: Version = required_str(root, "version", "the file")?
        .parse()
        .map_err(|e: lsr_core::pack::PackError| invalid(e.to_string()))?;
    let optional = |key: &str| -> Result<String, FormatError> {
        match root.get(key) {
            Some(_) => Ok(required_str(root, key, "the file")?.to_owned()),
            None => Ok(String::new()),
        }
    };
    let locked = match root.get("locked") {
        None => false,
        Some(item) => item
            .as_bool()
            .ok_or_else(|| invalid("`locked` must be true or false".into()))?,
    };
    let derived_from = match root.get("derived_from") {
        None => None,
        Some(_) => Some(
            required_str(root, "derived_from", "the file")?
                .parse::<PackRef>()
                .map_err(|e| invalid(format!("`derived_from`: {e} (write name@1.2.3)")))?,
        ),
    };

    let mut params = Vec::new();
    for (i, t) in tables(root.get("param"), "param")?.into_iter().enumerate() {
        let at = format!("param #{}", i + 1);
        only_keys(
            t,
            &["name", "label", "default", "min", "max", "help", "to"],
            &at,
        )?;
        let name = required_str(t, "name", &at)?.to_owned();
        let at = format!("parameter `{name}`");
        let default = t
            .get("default")
            .and_then(Item::as_value)
            .ok_or_else(|| invalid(format!("{at}: missing `default`")))
            .and_then(|v| toml_to_json(v).map_err(|e| invalid(format!("{at}: default: {e}"))))?;
        let limit = |key: &str| -> Result<Option<f64>, FormatError> {
            match t.get(key) {
                None => Ok(None),
                Some(item) => item
                    .as_float()
                    .or_else(|| item.as_integer().map(|i| i as f64))
                    .filter(|f| f.is_finite())
                    .map(Some)
                    .ok_or_else(|| invalid(format!("{at}: `{key}` must be a number"))),
            }
        };
        let text = |key: &str| -> Result<String, FormatError> {
            match t.get(key) {
                None => Ok(String::new()),
                Some(_) => Ok(required_str(t, key, &at)?.to_owned()),
            }
        };
        let mut targets = Vec::new();
        for entry in refs(t, "to", &at)? {
            only_keys(entry, &["node", "param"], &at)?;
            targets.push(ParamTarget::new(
                NodeId(required_u64(entry, "node", &at)?),
                required_str(entry, "param", &at)?,
            ));
        }
        params.push(PromotedParam {
            name,
            label: text("label")?,
            default,
            min: limit("min")?,
            max: limit("max")?,
            help: text("help")?,
            targets,
        });
    }

    let mut inputs = Vec::new();
    for (i, t) in tables(root.get("input"), "input")?.into_iter().enumerate() {
        let at = format!("input #{}", i + 1);
        only_keys(t, &["name", "to"], &at)?;
        let name = required_str(t, "name", &at)?.to_owned();
        let at = format!("input `{name}`");
        let mut targets = Vec::new();
        for entry in refs(t, "to", &at)? {
            only_keys(entry, &["node", "input"], &at)?;
            targets.push(Endpoint::new(
                NodeId(required_u64(entry, "node", &at)?),
                required_str(entry, "input", &at)?,
            ));
        }
        inputs.push(PackInput { name, targets });
    }

    let mut outputs = Vec::new();
    for (i, t) in tables(root.get("output"), "output")?
        .into_iter()
        .enumerate()
    {
        let at = format!("output #{}", i + 1);
        only_keys(t, &["name", "from"], &at)?;
        let name = required_str(t, "name", &at)?.to_owned();
        let at = format!("output `{name}`");
        let from = t
            .get("from")
            .ok_or_else(|| invalid(format!("{at}: missing `from`")))
            .and_then(|item| table_like(item, &format!("{at}: from")))?;
        only_keys(from, &["node", "output"], &at)?;
        outputs.push(PackOutput {
            name,
            source: Endpoint::new(
                NodeId(required_u64(from, "node", &at)?),
                required_str(from, "output", &at)?,
            ),
        });
    }

    Ok(PackDef {
        name,
        version,
        label: optional("label")?,
        summary: optional("summary")?,
        help: String::new(),
        locked,
        derived_from,
        params,
        inputs,
        outputs,
        network: Default::default(),
    })
}

/// `to = [{ ... }, ...]`: a list of inline tables (or one table).
fn refs<'a>(
    t: &'a dyn TableLike,
    key: &str,
    at: &str,
) -> Result<Vec<&'a dyn TableLike>, FormatError> {
    match t.get(key) {
        None => Err(invalid(format!("{at}: missing `{key}`"))),
        Some(Item::Value(Value::InlineTable(one))) => Ok(vec![one as &dyn TableLike]),
        Some(item) => tables(Some(item), key).map_err(|_| {
            invalid(format!(
                "{at}: `{key}` must be a list like [{{ node = 0, ... }}]"
            ))
        }),
    }
}

/// The project file a Pack error is about (`packs/glow/network.toml` for
/// a problem in the network inside, `packs/glow/pack.toml` otherwise).
pub fn error_file(e: &lsr_core::pack::PackError) -> Option<String> {
    use lsr_core::pack::PackError as E;
    match e {
        E::Invalid { pack, .. } | E::Locked(pack) => Some(format!("packs/{pack}/pack.toml")),
        E::Graph { pack, .. } => Some(format!("packs/{pack}/network.toml")),
        E::InvalidName(_)
        | E::InvalidVersion(_)
        | E::Unknown(_)
        | E::Cycle(_)
        | E::TooDeep(_)
        | E::Network(_) => None,
    }
}

/// The text files of one Pack folder: `(file name, contents)`.
pub fn pack_files(def: &PackDef) -> Result<Vec<(&'static str, String)>, FormatError> {
    let mut files = vec![
        ("pack.toml", pack_to_string(def)?),
        ("network.toml", network_to_string(&def.network)?),
    ];
    if !def.help.is_empty() {
        files.push(("help.md", def.help.clone()));
    }
    Ok(files)
}

/// Reads one Pack folder. `rel` names it in errors (`packs/glow`).
pub fn load_pack(dir: &Path, rel: &str) -> Result<PackDef, FormatError> {
    let read = |file: &str| -> Result<Option<String>, FormatError> {
        let path = dir.join(file);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&path)(e)),
        }
    };
    let in_file = |file: &str, e: FormatError| e.in_file(&format!("{rel}/{file}"));
    let text = read("pack.toml")?.ok_or_else(|| FormatError::InFile {
        file: format!("{rel}/pack.toml"),
        message: "missing (a Pack folder needs pack.toml and network.toml)".into(),
    })?;
    let mut def = pack_from_str(&text).map_err(|e| in_file("pack.toml", e))?;
    let net = read("network.toml")?.ok_or_else(|| FormatError::InFile {
        file: format!("{rel}/network.toml"),
        message: "missing (the network inside the Pack)".into(),
    })?;
    def.network = network_from_str(&net).map_err(|e| in_file("network.toml", e))?;
    def.help = read("help.md")?.unwrap_or_default();
    Ok(def)
}

/// Writes one Pack folder, creating it if needed; removes a stale
/// `help.md` when the Pack has no help.
pub fn save_pack(dir: &Path, def: &PackDef) -> Result<(), FormatError> {
    let files = pack_files(def)?;
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    for (name, text) in &files {
        write_atomic(&dir.join(name), text)?;
    }
    let help = dir.join("help.md");
    if def.help.is_empty() && help.exists() {
        std::fs::remove_file(&help).map_err(io(&help))?;
    }
    Ok(())
}

/// Removes a Pack folder written by [`save_pack`]: its own files, then the
/// folder if nothing else is in it.
pub(crate) fn remove_pack(dir: &Path) -> Result<(), FormatError> {
    for f in FILES {
        let path = dir.join(f);
        if path.exists() {
            std::fs::remove_file(&path).map_err(io(&path))?;
        }
    }
    if std::fs::read_dir(dir).map_err(io(dir))?.next().is_none() {
        std::fs::remove_dir(dir).map_err(io(dir))?;
    }
    Ok(())
}

/// Sub-folders of `dir` (sorted), or none if `dir` does not exist.
pub(crate) fn subdirs(dir: &Path) -> Result<Vec<PathBuf>, FormatError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(io(dir))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()
        .map_err(io(dir))?;
    out.retain(|p| p.is_dir());
    out.sort();
    Ok(out)
}

/// A folder of Packs that projects copy from (the repository's `packs/`,
/// or a user's own). Several versions of one Pack may sit side by side:
/// `glow/` and `glow@1.0.0/`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Library {
    packs: BTreeMap<(String, Version), (PathBuf, PackDef)>,
}

impl Library {
    /// Reads every Pack folder under `dir`. A folder must be named after
    /// its Pack (`glow`, or `glow@1.0.0` for that exact version); other
    /// folders without a `pack.toml` are ignored.
    pub fn open(dir: &Path) -> Result<Self, FormatError> {
        let mut lib = Library::default();
        for path in subdirs(dir)? {
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
            let expected = [def.name.clone(), def.reference().to_string()];
            if !expected.contains(&folder) {
                return Err(FormatError::InFile {
                    file: format!("{rel}/pack.toml"),
                    message: format!(
                        "the folder of Pack {} must be named `{}` or `{}`",
                        def.reference(),
                        expected[0],
                        expected[1]
                    ),
                });
            }
            let key = (def.name.clone(), def.version);
            if lib.packs.contains_key(&key) {
                return Err(FormatError::InFile {
                    file: format!("{rel}/pack.toml"),
                    message: format!("{} is in the library twice", def.reference()),
                });
            }
            lib.packs.insert(key, (path, def));
        }
        Ok(lib)
    }

    /// Every Pack and version, by name then version.
    pub fn iter(&self) -> impl Iterator<Item = &PackDef> {
        self.packs.values().map(|(_, d)| d)
    }

    /// The newest version of each Pack, by name.
    pub fn latest(&self) -> impl Iterator<Item = &PackDef> {
        let mut last: BTreeMap<&str, &PackDef> = BTreeMap::new();
        for d in self.iter() {
            last.insert(&d.name, d);
        }
        last.into_values()
    }

    /// The folder a Pack was read from.
    pub fn path(&self, name: &str, version: Version) -> Option<&Path> {
        self.packs
            .get(&(name.to_owned(), version))
            .map(|(p, _)| p.as_path())
    }

    /// The newest version of `name` that `req` accepts.
    pub fn find(&self, name: &str, req: VersionReq) -> Result<&PackDef, FormatError> {
        let versions: Vec<&PackDef> = self.iter().filter(|d| d.name == name).collect();
        if versions.is_empty() {
            return Err(invalid(format!("the library has no Pack `{name}`")));
        }
        versions
            .iter()
            .rev()
            .find(|d| req.matches(d.version))
            .copied()
            .ok_or_else(|| {
                let have: Vec<String> = versions.iter().map(|d| d.version.to_string()).collect();
                invalid(format!(
                    "Pack `{name}` {req} is not in the library (it has {})",
                    have.join(", ")
                ))
            })
    }

    /// Parses `name` or `name@<req>` and finds it.
    pub fn find_spec(&self, spec: &str) -> Result<&PackDef, FormatError> {
        let (name, req) = match spec.split_once('@') {
            Some((n, r)) => (
                n,
                r.parse::<VersionReq>()
                    .map_err(|e| invalid(e.to_string()))?,
            ),
            None => (spec, VersionReq::Any),
        };
        self.find(name, req)
    }

    pub fn len(&self) -> usize {
        self.packs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }
}
