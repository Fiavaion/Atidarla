//! `lasair docs`: operator reference pages generated from the registry
//! (docs-as-code, PRD M5; the same corpus grounds the assistant).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, bail};
use lsr_core::registry::{OpFamily, OperatorSchema, ParamKind, Registry};

fn kind(k: &ParamKind) -> String {
    match k {
        ParamKind::Float { .. } => "float".into(),
        ParamKind::Int { .. } => "int".into(),
        ParamKind::String => "string".into(),
        ParamKind::Menu { .. } => "menu".into(),
        ParamKind::Toggle => "toggle".into(),
        ParamKind::Pulse => "pulse".into(),
        ParamKind::Colour => "colour".into(),
        ParamKind::Vec { n } => format!("vec{n}"),
        ParamKind::Path => "path".into(),
    }
}

fn range(k: &ParamKind) -> String {
    let fmt = |min: Option<String>, max: Option<String>| match (min, max) {
        (None, None) => String::new(),
        (Some(a), None) => format!("≥ {a}"),
        (None, Some(b)) => format!("≤ {b}"),
        (Some(a), Some(b)) => format!("{a} – {b}"),
    };
    match k {
        ParamKind::Float { min, max } => {
            fmt(min.map(|v| v.to_string()), max.map(|v| v.to_string()))
        }
        ParamKind::Int { min, max } => fmt(min.map(|v| v.to_string()), max.map(|v| v.to_string())),
        ParamKind::Menu { options } => options
            .iter()
            .map(|o| format!("`{o}`"))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Markdown page for one operator.
pub fn page(op: &OperatorSchema, examples: &[Example]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# {} (`{}`)\n", op.label, op.type_id);
    let _ = writeln!(
        s,
        "{} · version {} · cook budget {} µs\n",
        op.family, op.version, op.cook_budget_us
    );
    let _ = writeln!(s, "{}\n", op.summary);
    if op.time_dependent {
        let _ = writeln!(s, "Cooks every frame (time-dependent).\n");
    }
    if op.sink {
        let _ = writeln!(
            s,
            "Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.\n"
        );
    }

    for (title, sockets) in [("Inputs", &op.inputs), ("Outputs", &op.outputs)] {
        let _ = writeln!(s, "## {title}\n");
        if sockets.is_empty() {
            let _ = writeln!(s, "None.\n");
            continue;
        }
        let _ = writeln!(s, "| Name | Type | Notes |\n|---|---|---|");
        for sock in sockets.iter() {
            let mut notes = Vec::new();
            if sock.feedback {
                notes.push("feedback: reads the source's previous frame");
            }
            if sock.optional && title == "Inputs" {
                notes.push("optional");
            }
            let _ = writeln!(
                s,
                "| `{}` | {:?} | {} |",
                sock.name,
                sock.wire,
                notes.join("; ")
            );
        }
        s.push('\n');
    }

    let _ = writeln!(s, "## Parameters\n");
    if op.params.is_empty() {
        let _ = writeln!(s, "None.\n");
    } else {
        let _ = writeln!(
            s,
            "| Name | Label | Type | Default | Range / options | Description |\n|---|---|---|---|---|---|"
        );
        for p in &op.params {
            let _ = writeln!(
                s,
                "| `{}` | {} | {} | `{}` | {} | {} |",
                p.name,
                cell(&p.label),
                kind(&p.kind),
                p.default,
                range(&p.kind),
                cell(&p.help)
            );
        }
        s.push('\n');
    }

    let _ = writeln!(s, "## Platforms\n");
    if op.capabilities.is_empty() {
        let _ = writeln!(s, "All platforms.\n");
    } else {
        for c in &op.capabilities {
            let _ = writeln!(
                s,
                "- Requires `{}`; elsewhere: {}",
                c.flag,
                c.fallback.explanation()
            );
        }
        s.push('\n');
    }

    let _ = writeln!(s, "## Examples\n");
    if examples.is_empty() {
        let _ = writeln!(s, "No example project uses it yet.");
    }
    for e in examples {
        let nodes: Vec<String> = e.nodes.iter().map(|n| format!("`{n}`")).collect();
        let _ = writeln!(
            s,
            "- [{}](../../../examples/{}/) — {}",
            e.title,
            e.folder,
            nodes.join(", ")
        );
    }
    s
}

/// An example project that uses an operator.
#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    /// Folder name under `examples/`.
    pub folder: String,
    pub title: String,
    /// Names of the nodes of this type.
    pub nodes: Vec<String>,
}

/// Example projects by operator type, from the `*.lasair` folders in
/// `dir` (in folder order).
pub fn examples_by_type(dir: &Path) -> anyhow::Result<BTreeMap<String, Vec<Example>>> {
    let mut folders: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == lsr_format::EXTENSION))
        .collect();
    folders.sort();
    let mut by_type: BTreeMap<String, Vec<Example>> = BTreeMap::new();
    for folder in folders {
        let project =
            lsr_format::load(&folder).with_context(|| format!("loading {}", folder.display()))?;
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for net in project.networks.values() {
            for node in net.nodes() {
                let list = by_type.entry(node.type_id.clone()).or_default();
                match list.iter_mut().find(|e| e.folder == name) {
                    Some(e) => e.nodes.push(node.name.clone()),
                    None => list.push(Example {
                        folder: name.clone(),
                        title: project.name.clone(),
                        nodes: vec![node.name.clone()],
                    }),
                }
            }
        }
    }
    Ok(by_type)
}

/// All generated files, keyed by path relative to the docs root.
pub fn generate(
    registry: &Registry,
    examples: &BTreeMap<String, Vec<Example>>,
) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut index = String::from(
        "# Operator reference\n\nGenerated by `lasair docs` from the operator registry. Do not edit by hand.\n\n",
    );
    for family in OpFamily::ALL {
        let ops: Vec<&OperatorSchema> = registry.family(family).collect();
        let _ = writeln!(index, "## {family} ({})\n", ops.len());
        if ops.is_empty() {
            let _ = writeln!(index, "No operators yet.\n");
            continue;
        }
        for op in ops {
            let (_, name) = op.type_id.split_once('.').expect("valid type id");
            let rel = format!("{}/{name}.md", family.prefix());
            let _ = writeln!(index, "- [{}]({rel}) — {}", op.label, op.summary);
            let used = examples.get(&op.type_id).map_or(&[][..], Vec::as_slice);
            files.insert(rel, page(op, used));
        }
        index.push('\n');
    }
    files.insert("README.md".into(), index);
    files
}

/// Writes the pages under `out`, or with `check` verifies they are current.
pub fn run(registry: &Registry, out: &Path, examples: &Path, check: bool) -> anyhow::Result<()> {
    let files = generate(registry, &examples_by_type(examples)?);
    let mut stale = Vec::new();
    for (rel, content) in &files {
        let path = out.join(rel);
        if check {
            let current = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .replace("\r\n", "\n");
            if current != *content {
                stale.push(rel.clone());
            }
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, content)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }
    if check && !stale.is_empty() {
        bail!(
            "operator docs are out of date ({}); run `cargo run -p lsr-cli -- docs`",
            stale.join(", ")
        );
    }
    if !check {
        println!("wrote {} files to {}", files.len(), out.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_cover_every_operator() {
        let reg = crate::builtin_registry();
        let examples = BTreeMap::from([(
            "img.constant".to_owned(),
            vec![Example {
                folder: "a.lasair".into(),
                title: "A".into(),
                nodes: vec!["solid".into(), "black".into()],
            }],
        )]);
        let files = generate(&reg, &examples);
        assert_eq!(files.len(), reg.len() + 1);
        let constant = &files["img/constant.md"];
        assert!(constant.contains("# Constant (`img.constant`)"));
        assert!(constant.contains("| `colour` | Colour | colour |"));
        assert!(constant.contains("- [A](../../../examples/a.lasair/) — `solid`, `black`"));
        assert!(files["img/blur.md"].contains("No example project uses it yet."));
    }
}
