//! `lasair export-show` and `lasair verify-show`: a project as one
//! portable show bundle (FR-S9, M3 AC2). Usage and the bundle layout:
//! `docs/show-export.md`.
//!
//! Export copies the project folder, copies every file a path parameter
//! names from outside the project into `media/` (deduplicated by content,
//! names kept), rewrites those parameters as relative paths with `/`
//! separators, and writes `bundle.toml`: every file with its SHA-256, the
//! nodes that need a platform capability (PRD A19) and a placeholder for
//! the model manifest (M4). The copy is then verified as it would be on the
//! receiving machine: every path parameter relative, inside the bundle and
//! spelled with the file's exact case, every hash matching, and `check`
//! (with `--gpu`, cooking every node) passing.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use lsr_core::graph::{Edit, Network, NodeId};
use lsr_core::registry::{ParamKind, Registry};
use sha2::{Digest, Sha256};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

/// The manifest at the bundle's root.
pub const MANIFEST: &str = "bundle.toml";
/// Where files from outside the project go.
const MEDIA: &str = "media";
/// Bundle format version.
const FORMAT: i64 = 1;

/// Path parameters that name a file the node writes, not one it reads:
/// rewritten into the bundle (under `out/`) but not copied.
const OUTPUTS: &[(&str, &str)] = &[("msh.file_out", "file")];

#[derive(Debug, Clone, Copy)]
pub struct ExportOptions {
    /// Also write `<out>/<name>.zip`.
    pub zip: bool,
    /// Verify with `check --gpu` (cook every node) rather than without.
    pub gpu: bool,
}

/// What an export produced.
#[derive(Debug)]
pub struct Exported {
    pub bundle: PathBuf,
    pub zip: Option<PathBuf>,
    /// Files in the bundle (manifest excluded).
    pub files: usize,
    /// Files copied in from outside the project.
    pub external: usize,
}

/// Runs `export-show` and prints a summary.
pub fn run(project: &Path, out: &Path, opts: ExportOptions) -> Result<()> {
    let e = export(project, out, opts)?;
    println!(
        "exported {} ({} files, {} copied in from outside the project){}",
        e.bundle.display(),
        e.files,
        e.external,
        e.zip
            .as_ref()
            .map(|z| format!(", zipped to {}", z.display()))
            .unwrap_or_default()
    );
    Ok(())
}

/// One path parameter of one node.
#[derive(Debug, Clone)]
struct Ref {
    /// `networks/<id>.toml` or `packs/<name>/`.
    owner: Owner,
    node: NodeId,
    node_name: String,
    param: String,
    text: String,
    output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Network(String),
    Pack(String),
}

impl std::fmt::Display for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Owner::Network(id) => write!(f, "networks/{id}.toml"),
            Owner::Pack(name) => write!(f, "packs/{name}"),
        }
    }
}

/// Every non-empty path parameter set on a node of `net`.
fn path_refs(owner: &Owner, net: &Network, registry: &Registry, out: &mut Vec<Ref>) {
    for node in net.nodes() {
        let Some(schema) = registry.get(&node.type_id) else {
            continue;
        };
        for p in &schema.params {
            if p.kind != ParamKind::Path {
                continue;
            }
            let Some(text) = node.params.get(&p.name).and_then(|v| v.value.as_str()) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            out.push(Ref {
                owner: owner.clone(),
                node: node.id,
                node_name: node.name.clone(),
                param: p.name.clone(),
                text: text.to_owned(),
                output: OUTPUTS.contains(&(node.type_id.as_str(), p.name.as_str())),
            });
        }
    }
}

fn all_refs(project: &lsr_format::Project, registry: &Registry) -> Vec<Ref> {
    let mut refs = Vec::new();
    for (id, net) in &project.networks {
        path_refs(&Owner::Network(id.clone()), net, registry, &mut refs);
    }
    for def in project.packs.iter() {
        path_refs(
            &Owner::Pack(def.name.clone()),
            &def.network,
            registry,
            &mut refs,
        );
    }
    refs
}

/// SHA-256 of a file, as lower-case hex, and its size.
pub fn sha256(path: &Path) -> Result<(String, u64)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((hex, size))
}

/// `path` relative to `root` with `/` separators, if it is inside.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A parameter's text as a path on this machine: absolute as it is,
/// relative to the project folder; backslashes written on Windows are
/// separators everywhere.
fn resolve(project: &Path, text: &str) -> PathBuf {
    let p = Path::new(text);
    if p.is_absolute() {
        return p.to_owned();
    }
    let portable = text.replace('\\', "/");
    project.join(portable)
}

/// URIs of the files a glTF (JSON) file loads besides itself.
fn gltf_companions(path: &Path) -> Result<Vec<String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("{}: not glTF JSON", path.display()))?;
    let mut uris = Vec::new();
    for key in ["buffers", "images"] {
        for item in json
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let Some(uri) = item.get("uri").and_then(|u| u.as_str()) else {
                continue;
            };
            if uri.starts_with("data:") {
                continue;
            }
            let safe = !Path::new(uri).is_absolute()
                && !uri.contains(':')
                && Path::new(uri)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)));
            if !safe {
                bail!(
                    "{}: `{uri}` must be a relative path inside the file's folder to be bundled",
                    path.display()
                );
            }
            uris.push(uri.to_owned());
        }
    }
    uris.sort();
    uris.dedup();
    Ok(uris)
}

/// Chooses bundle names for files from outside the project: the file's own
/// name in `media/`, deduplicated by content; a different file with a
/// name already taken gets `-2`, `-3`, ... before the extension.
#[derive(Debug, Default)]
struct Media {
    /// Bundle path → content hash.
    taken: BTreeMap<String, String>,
    /// Source file → bundle path.
    by_source: BTreeMap<PathBuf, String>,
    /// (source, bundle path) to copy.
    copies: Vec<(PathBuf, String)>,
}

impl Media {
    fn claim(&mut self, project: &Path, rel: &str, hash: &str) -> Result<bool> {
        if let Some(h) = self.taken.get(rel) {
            return Ok(h == hash);
        }
        // A file of the project's own media/ folder is copied with it.
        let existing = project.join(rel);
        if existing.exists() {
            let (h, _) = sha256(&existing)?;
            let same = h == hash;
            self.taken.insert(rel.to_owned(), h);
            return Ok(same);
        }
        self.taken.insert(rel.to_owned(), hash.to_owned());
        Ok(true)
    }

    /// The bundle path for `source` (canonical), copying it in.
    fn place(&mut self, project: &Path, source: &Path) -> Result<String> {
        if let Some(rel) = self.by_source.get(source) {
            return Ok(rel.clone());
        }
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .with_context(|| format!("{}: no file name", source.display()))?;
        let gltf = source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gltf"));
        let rel = if gltf {
            self.place_gltf(project, source, &name)?
        } else {
            let (hash, _) = sha256(source)?;
            let (stem, ext) = split_name(&name);
            let mut n = 1;
            loop {
                let candidate = if n == 1 {
                    format!("{MEDIA}/{name}")
                } else {
                    format!("{MEDIA}/{stem}-{n}{ext}")
                };
                if self.claim(project, &candidate, &hash)? {
                    if !self.copies.iter().any(|(_, r)| *r == candidate)
                        && !project.join(&candidate).exists()
                    {
                        self.copies.push((source.to_owned(), candidate.clone()));
                    }
                    break candidate;
                }
                n += 1;
            }
        };
        self.by_source.insert(source.to_owned(), rel.clone());
        Ok(rel)
    }

    /// A glTF file and the buffers and images it loads go into a folder of
    /// their own, `media/<stem>/`, keeping their relative layout.
    fn place_gltf(&mut self, project: &Path, source: &Path, name: &str) -> Result<String> {
        let dir = source.parent().unwrap_or(Path::new("."));
        let companions = gltf_companions(source)?;
        let (stem, _) = split_name(name);
        let mut n = 1;
        loop {
            let folder = if n == 1 {
                format!("{MEDIA}/{stem}")
            } else {
                format!("{MEDIA}/{stem}-{n}")
            };
            let mut files = vec![(source.to_owned(), format!("{folder}/{name}"))];
            for uri in &companions {
                files.push((dir.join(uri), format!("{folder}/{uri}")));
            }
            let mut fits = true;
            for (src, rel) in &files {
                let (hash, _) = sha256(src)?;
                if !self.claim(project, rel, &hash)? {
                    fits = false;
                    break;
                }
            }
            if fits {
                for (src, rel) in files {
                    if !self.copies.iter().any(|(_, r)| *r == rel) && !project.join(&rel).exists() {
                        self.copies.push((src, rel));
                    }
                }
                return Ok(format!("{folder}/{name}"));
            }
            n += 1;
        }
    }
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Copies the project folder into `to`, leaving out `.cache/` (rebuilt on
/// open) and `bundle.toml` from an earlier export.
fn copy_tree(from: &Path, to: &Path, root: bool) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .with_context(|| format!("reading {}", from.display()))?
        .collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        if root && (name == ".cache" || name == MANIFEST) {
            continue;
        }
        let src = entry.path();
        let dst = to.join(&name);
        if std::fs::metadata(&src)?.is_dir() {
            copy_tree(&src, &dst, false)?;
        } else {
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
        }
    }
    Ok(())
}

/// Every file under `dir`, as sorted bundle paths.
fn list_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if std::fs::metadata(&path)?.is_dir() {
                walk(root, &path, out)?;
            } else if let Some(rel) = relative(root, &path) {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.retain(|f| f != MANIFEST);
    out.sort();
    Ok(out)
}

/// Exports `project` to `<out>/<project folder name>/` (and a zip).
pub fn export(project: &Path, out: &Path, opts: ExportOptions) -> Result<Exported> {
    let src = std::fs::canonicalize(project)
        .with_context(|| format!("{}: no such project folder", project.display()))?;
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .context("the project folder has no name")?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let out = std::fs::canonicalize(out)?;
    if out.starts_with(&src) {
        bail!(
            "{}: the output folder must be outside the project",
            out.display()
        );
    }
    let bundle = out.join(&name);
    if bundle.exists() && std::fs::read_dir(&bundle)?.next().is_some() {
        bail!(
            "{} already exists and is not empty; remove it or export elsewhere",
            bundle.display()
        );
    }

    let crate::project::Loaded {
        mut project,
        registry,
        ..
    } = crate::project::load_checked(&src)?;
    let refs = all_refs(&project, &registry);

    // Where each parameter will point inside the bundle.
    let mut media = Media::default();
    let mut missing = Vec::new();
    let mut rewrites: Vec<(Ref, String)> = Vec::new();
    for r in &refs {
        let path = resolve(&src, &r.text);
        let target = if r.output {
            // Written by the node: inside the project stays, outside goes to out/.
            let parent = path.parent().and_then(|p| std::fs::canonicalize(p).ok());
            match parent.as_deref().and_then(|p| relative(&src, p)) {
                Some(dir) => format!(
                    "{dir}/{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                None if parent.as_deref() == Some(src.as_path()) => path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                None => format!(
                    "out/{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
            }
        } else {
            let Ok(canonical) = std::fs::canonicalize(&path) else {
                missing.push(format!(
                    "{}: {}.{} = \"{}\"",
                    r.owner, r.node_name, r.param, r.text
                ));
                continue;
            };
            if canonical.is_dir() {
                bail!(
                    "{}: {}.{} names a folder ({}); export-show bundles files",
                    r.owner,
                    r.node_name,
                    r.param,
                    canonical.display()
                );
            }
            match relative(&src, &canonical) {
                Some(rel) => rel,
                None => media.place(&src, &canonical)?,
            }
        };
        if target != r.text {
            rewrites.push((r.clone(), target));
        }
    }
    if !missing.is_empty() {
        bail!(
            "files the project uses are missing, so the bundle would not play: {}",
            missing.join("; ")
        );
    }

    // Rewrite the parameters in memory, then write the copy.
    for (r, target) in &rewrites {
        let set = |net: &mut Network| -> Result<()> {
            let mut param = net
                .node(r.node)
                .and_then(|n| n.params.get(&r.param))
                .cloned()
                .context("parameter vanished")?;
            param.value = serde_json::Value::from(target.clone());
            net.apply(
                &registry,
                Edit::SetParam {
                    node: r.node,
                    param: r.param.clone(),
                    value: Some(param),
                },
            )?;
            Ok(())
        };
        match &r.owner {
            Owner::Network(id) => set(project.networks.get_mut(id).context("network vanished")?)?,
            Owner::Pack(pack) => {
                let mut def = project.packs.get(pack).cloned().context("Pack vanished")?;
                set(&mut def.network)?;
                project.packs.insert(def);
            }
        }
    }
    copy_tree(&src, &bundle, true)?;
    for (from, rel) in &media.copies {
        let dst = bundle.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, &dst)
            .with_context(|| format!("copying {} to {}", from.display(), dst.display()))?;
    }
    lsr_format::save(&bundle, &project)?;

    let external: Vec<String> = media.copies.iter().map(|(_, r)| r.clone()).collect();
    let files = write_manifest(&bundle, &project, &registry, &external)?;
    verify(&bundle, opts.gpu)?;
    let zip = if opts.zip {
        let path = out.join(format!("{name}.zip"));
        write_zip(&bundle, &name, &path)?;
        Some(path)
    } else {
        None
    };
    Ok(Exported {
        bundle,
        zip,
        files,
        external: external.len(),
    })
}

fn string(s: &str) -> Value {
    Value::from(s)
}

/// Writes `bundle.toml`; returns the number of files listed.
fn write_manifest(
    bundle: &Path,
    project: &lsr_format::Project,
    registry: &Registry,
    external: &[String],
) -> Result<usize> {
    let mut doc = DocumentMut::new();
    doc.insert("format", Item::Value(Value::from(FORMAT)));
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(
            "# Lasair show bundle, written by `lasair export-show` (docs/show-export.md).\n\
             # Open this folder as a project. `lasair verify-show` checks it.\n",
        );
    }
    doc.insert("name", Item::Value(string(&project.name)));
    doc.insert("lasair", Item::Value(string(lsr_core::VERSION)));
    doc.insert(
        "exported_on",
        Item::Value(string(&format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))),
    );

    let files = list_files(bundle)?;
    let mut list = ArrayOfTables::new();
    for f in &files {
        let (hash, size) = sha256(&bundle.join(f))?;
        let mut t = Table::new();
        t.insert("path", Item::Value(string(f)));
        t.insert("sha256", Item::Value(string(&hash)));
        t.insert("bytes", Item::Value(Value::from(size as i64)));
        if external.contains(f) {
            t.insert("external", Item::Value(Value::from(true)));
        }
        list.push(t);
    }
    doc.insert("file", Item::ArrayOfTables(list));

    // Nodes that need a platform capability, and what stands in for them
    // elsewhere (PRD A19).
    let mut nodes = ArrayOfTables::new();
    let mut add = |owner: &str, net: &Network| {
        for node in net.nodes() {
            let Some(schema) = registry.get(&node.type_id) else {
                continue;
            };
            for req in &schema.capabilities {
                let mut t = Table::new();
                t.insert("in", Item::Value(string(owner)));
                t.insert("node", Item::Value(string(&node.name)));
                t.insert("type", Item::Value(string(&node.type_id)));
                t.insert("needs", Item::Value(string(&req.flag.to_string())));
                let (kind, extra) = match &req.fallback {
                    lsr_core::capability::Fallback::RouteRemote { .. } => ("route_remote", None),
                    lsr_core::capability::Fallback::Degrade { .. } => ("degrade", None),
                    lsr_core::capability::Fallback::Substitute { op, .. } => {
                        ("substitute", Some(op.as_str()))
                    }
                    lsr_core::capability::Fallback::Hide { .. } => ("hide", None),
                };
                t.insert("fallback", Item::Value(string(kind)));
                if let Some(op) = extra {
                    t.insert("substitute", Item::Value(string(op)));
                }
                t.insert(
                    "explanation",
                    Item::Value(string(req.fallback.explanation())),
                );
                nodes.push(t);
            }
        }
    };
    for (id, net) in &project.networks {
        add(&format!("networks/{id}.toml"), net);
    }
    for def in project.packs.iter() {
        add(&format!("packs/{}", def.name), &def.network);
    }
    if !nodes.is_empty() {
        doc.insert("node", Item::ArrayOfTables(nodes));
    }

    let mut models = Table::new();
    models.insert(
        "required",
        Item::Value(Value::Array(toml_edit::Array::new())),
    );
    if let Some((mut key, _)) = models.get_key_value_mut("required") {
        key.leaf_decor_mut().set_prefix(
            "# The model manifest (M4): models the AI nodes need, with licences,\n\
             # fetched by the model manager on open. Weights are never bundled.\n",
        );
    }
    doc.insert("models", Item::Table(models));
    std::fs::write(bundle.join(MANIFEST), doc.to_string())
        .with_context(|| format!("writing {MANIFEST}"))?;
    Ok(files.len())
}

/// True when `rel` (with `/`) exists under `root` with exactly this case
/// in every component, as a case-sensitive file system would need.
fn exists_exact(root: &Path, rel: &str) -> bool {
    let mut dir = root.to_owned();
    for part in rel.split('/') {
        let found = std::fs::read_dir(&dir).ok().is_some_and(|mut entries| {
            entries.any(|e| e.is_ok_and(|e| e.file_name().to_str() == Some(part)))
        });
        if !found {
            return false;
        }
        dir.push(part);
    }
    true
}

/// Checks a bundle as the receiving machine would: path parameters are
/// relative, inside it and exactly spelled; the manifest lists every file
/// with the right hash; and `check` passes (with `gpu`, every node cooks).
pub fn verify(bundle: &Path, gpu: bool) -> Result<()> {
    let crate::project::Loaded {
        project, registry, ..
    } = crate::project::load_checked(bundle)?;
    let mut bad = Vec::new();
    for r in all_refs(&project, &registry) {
        let t = &r.text;
        let portable = !Path::new(t).is_absolute()
            && !t.contains('\\')
            && !t.contains(':')
            && Path::new(t)
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        let what = format!("{}: {}.{} = \"{t}\"", r.owner, r.node_name, r.param);
        if !portable {
            bad.push(format!("{what} is not a relative path inside the bundle"));
        } else if !r.output && !exists_exact(bundle, t) {
            bad.push(format!(
                "{what} does not exist in the bundle (with exactly this case)"
            ));
        }
    }
    if !bad.is_empty() {
        bail!("{}: {}", bundle.display(), bad.join("; "));
    }

    let text = std::fs::read_to_string(bundle.join(MANIFEST))
        .with_context(|| format!("{}: no {MANIFEST}", bundle.display()))?;
    let doc: DocumentMut = text
        .parse()
        .with_context(|| format!("{}: {MANIFEST} is not TOML", bundle.display()))?;
    let mut listed = BTreeMap::new();
    if let Some(files) = doc.get("file").and_then(Item::as_array_of_tables) {
        for t in files {
            let path = t.get("path").and_then(Item::as_str).unwrap_or_default();
            let hash = t.get("sha256").and_then(Item::as_str).unwrap_or_default();
            listed.insert(path.to_owned(), hash.to_owned());
        }
    }
    let on_disk = list_files(bundle)?;
    let mut problems = Vec::new();
    for f in &on_disk {
        match listed.get(f) {
            None => problems.push(format!("{f} is not in {MANIFEST}")),
            Some(want) => {
                let (got, _) = sha256(&bundle.join(f))?;
                if &got != want {
                    problems.push(format!("{f} differs from its SHA-256 in {MANIFEST}"));
                }
            }
        }
    }
    for f in listed.keys() {
        if !on_disk.contains(f) {
            problems.push(format!("{f} is listed in {MANIFEST} but missing"));
        }
    }
    if !problems.is_empty() {
        bail!("{}: {}", bundle.display(), problems.join("; "));
    }
    crate::project::check(
        &[bundle],
        crate::project::CheckOptions {
            gpu,
            frames: 30,
            allow_python_ops: false,
        },
    )
}

/// Zips the bundle as `<name>/...`, files in sorted order with a fixed
/// timestamp, so the same bundle always gives the same archive. Media
/// that is already compressed is stored.
fn write_zip(bundle: &Path, name: &str, path: &Path) -> Result<()> {
    use zip::write::SimpleFileOptions;
    const STORED: &[&str] = &[
        "mov", "mp4", "m4v", "mkv", "webm", "avi", "png", "jpg", "jpeg", "gif", "webp", "mp3",
        "m4a", "aac", "ogg", "flac", "zip", "glb", "exr",
    ];
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let mut files = list_files(bundle)?;
    files.push(MANIFEST.to_owned());
    files.sort();
    for f in files {
        let src = bundle.join(&f);
        let size = std::fs::metadata(&src)?.len();
        let stored = Path::new(&f)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| STORED.contains(&e.to_ascii_lowercase().as_str()));
        let options = SimpleFileOptions::default()
            .compression_method(if stored {
                zip::CompressionMethod::Stored
            } else {
                zip::CompressionMethod::Deflated
            })
            .last_modified_time(zip::DateTime::default())
            .large_file(size >= u64::from(u32::MAX))
            .unix_permissions(0o644);
        zip.start_file(format!("{name}/{f}"), options)?;
        let mut input = std::fs::File::open(&src)?;
        std::io::copy(&mut input, &mut zip)
            .with_context(|| format!("zipping {}", src.display()))?;
    }
    zip.finish()?.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsr_core::graph::{Node, Param};
    use serde_json::json;

    struct Temp(PathBuf);

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("lsr-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temp(std::fs::canonicalize(&dir).unwrap())
    }

    fn data(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lsr-io/tests/data")
            .join(name)
    }

    /// A project whose nodes name files outside it by absolute path (two
    /// different files both called `clip.mov`, one file twice), one inside
    /// it by relative path with Windows separators, and a written file.
    fn project(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let ext_a = root.join("footage/a");
        let ext_b = root.join("footage/b");
        std::fs::create_dir_all(&ext_a).unwrap();
        std::fs::create_dir_all(&ext_b).unwrap();
        std::fs::copy(data("hap1.mov"), ext_a.join("clip.mov")).unwrap();
        std::fs::copy(data("hap_alpha.mov"), ext_b.join("clip.mov")).unwrap();
        std::fs::copy(data("still.png"), ext_a.join("still.png")).unwrap();

        let dir = root.join("show.lasair");
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::copy(data("still.jpg"), dir.join("media/inside.jpg")).unwrap();
        let (registry, _) = crate::project::library();
        let mut net = Network::default();
        let mut add = |ty: &str, name: &str, file: &str| {
            let id = net.alloc_id();
            net.apply(
                &registry,
                Edit::AddNode {
                    node: Node::new(id, ty, name),
                },
            )
            .unwrap();
            net.apply(
                &registry,
                Edit::SetParam {
                    node: id,
                    param: "file".into(),
                    value: Some(Param::constant(json!(file))),
                },
            )
            .unwrap();
        };
        let a = ext_a.join("clip.mov");
        let b = ext_b.join("clip.mov");
        let still = ext_a.join("still.png");
        add("img.movie_in", "movie_a", a.to_str().unwrap());
        add("img.movie_in", "movie_b", b.to_str().unwrap());
        add("img.movie_in", "movie_a_again", a.to_str().unwrap());
        add("img.movie_in", "still", still.to_str().unwrap());
        add("img.movie_in", "inside", "media\\inside.jpg");
        add(
            "msh.file_out",
            "writer",
            root.join("exports/out.obj").to_str().unwrap(),
        );
        let mut project = lsr_format::Project::with_main("Show", net);
        project.resolution = [64, 36];
        lsr_format::save(&dir, &project).unwrap();
        std::fs::create_dir_all(dir.join(".cache")).unwrap();
        std::fs::write(dir.join(".cache/junk.bin"), b"cache").unwrap();
        (dir, a, b)
    }

    fn params(bundle: &Path) -> BTreeMap<String, String> {
        let p = lsr_format::load(bundle).unwrap();
        p.main()
            .unwrap()
            .nodes()
            .map(|n| {
                let v = n.params["file"].value.as_str().unwrap().to_owned();
                (n.name.clone(), v)
            })
            .collect()
    }

    #[test]
    fn exports_external_media_as_relative_paths_with_matching_hashes() {
        let root = temp("media");
        let (dir, a, b) = project(&root.0);
        let out = root.0.join("dist");
        let e = export(
            &dir,
            &out,
            ExportOptions {
                zip: true,
                gpu: false,
            },
        )
        .unwrap();
        assert_eq!(e.bundle, out.join("show.lasair"));
        let p = params(&e.bundle);
        assert_eq!(p["movie_a"], "media/clip.mov");
        assert_eq!(p["movie_b"], "media/clip-2.mov", "same name, other content");
        assert_eq!(p["movie_a_again"], "media/clip.mov", "deduplicated");
        assert_eq!(p["still"], "media/still.png");
        assert_eq!(p["inside"], "media/inside.jpg", "separators made portable");
        assert_eq!(p["writer"], "out/out.obj", "written files are not copied");
        assert!(p.values().all(|v| !Path::new(v).is_absolute()));
        assert_eq!(e.external, 3);
        assert_eq!(
            sha256(&e.bundle.join("media/clip.mov")).unwrap(),
            sha256(&a).unwrap()
        );
        assert_eq!(
            sha256(&e.bundle.join("media/clip-2.mov")).unwrap(),
            sha256(&b).unwrap()
        );
        assert!(!e.bundle.join(".cache").exists(), "the cache stays behind");
        // The manifest lists every file with its hash.
        let manifest = std::fs::read_to_string(e.bundle.join(MANIFEST)).unwrap();
        let (hash, _) = sha256(&a).unwrap();
        assert!(manifest.contains(&format!("path = \"media/clip.mov\"\nsha256 = \"{hash}\"")));
        assert!(manifest.contains("path = \"networks/main.toml\""));
        assert!(manifest.contains("[models]"));
        // The zip holds the bundle under its folder name.
        let zip = zip::ZipArchive::new(std::fs::File::open(e.zip.unwrap()).unwrap()).unwrap();
        let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        assert!(names.iter().any(|n| n == "show.lasair/media/clip-2.mov"));
        assert!(names.iter().any(|n| n == "show.lasair/bundle.toml"));

        // Moved elsewhere, with the originals gone, it still verifies and
        // loads: nothing points outside it.
        let moved = root.0.join("elsewhere/show.lasair");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&e.bundle, &moved).unwrap();
        std::fs::remove_dir_all(root.0.join("footage")).unwrap();
        verify(&moved, false).unwrap();
        let loaded = lsr_format::load(&moved).unwrap();
        assert_eq!(loaded.main().unwrap().len(), 6);

        // Tampering is caught.
        std::fs::write(moved.join("media/still.png"), b"not the same").unwrap();
        let err = verify(&moved, false).unwrap_err().to_string();
        assert!(err.contains("media/still.png differs"), "{err}");
    }

    #[test]
    fn refuses_missing_files_and_unsafe_outputs() {
        let root = temp("missing");
        let (dir, a, _) = project(&root.0);
        std::fs::remove_file(&a).unwrap();
        let err = export(
            &dir,
            &root.0.join("dist"),
            ExportOptions {
                zip: false,
                gpu: false,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing") && err.contains("movie_a"), "{err}");
        let err = export(
            &dir,
            &dir.join("inside"),
            ExportOptions {
                zip: false,
                gpu: false,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("outside the project"), "{err}");
    }

    #[test]
    fn exports_examples_with_their_project_files() {
        // An example with project-local files (patch, fixtures): nothing
        // external, every file listed, the copy verifies, cooking every
        // node where there is a GPU.
        let root = temp("example");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/pixel-map.lasair");
        let gpu = lsr_gpu::GpuContext::headless().is_ok();
        let e = export(&src, &root.0, ExportOptions { zip: false, gpu }).unwrap();
        assert_eq!(e.external, 0);
        let manifest = std::fs::read_to_string(e.bundle.join(MANIFEST)).unwrap();
        for f in ["patch.toml", "fixtures/led-bar.toml", "project.toml"] {
            assert!(manifest.contains(&format!("path = \"{f}\"")), "{f}");
        }
        assert!(exists_exact(&e.bundle, "fixtures/led-bar.toml"));
        assert!(!exists_exact(&e.bundle, "Fixtures/led-bar.toml"));
    }

    #[test]
    fn gltf_files_bring_their_buffers() {
        let root = temp("gltf");
        let ext = root.0.join("models");
        std::fs::create_dir_all(ext.join("bin")).unwrap();
        std::fs::write(
            ext.join("tri.gltf"),
            r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"bin/tri.bin","byteLength":4}]}"#,
        )
        .unwrap();
        std::fs::write(ext.join("bin/tri.bin"), [0u8, 1, 2, 3]).unwrap();
        let mut media = Media::default();
        let rel = media.place(&root.0, &ext.join("tri.gltf")).unwrap();
        assert_eq!(rel, "media/tri/tri.gltf");
        let copied: Vec<&str> = media.copies.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(copied, ["media/tri/tri.gltf", "media/tri/bin/tri.bin"]);
    }
}
