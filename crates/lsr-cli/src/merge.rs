//! `lasair diff`, `merge` and `merge-file` (FR-F2, FR-F3, docs/merge.md).
//!
//! Exit status: 0 no conflicts, 1 conflicts, 2 an error (a side that does
//! not load, a file that does not parse).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lsr_format::git::Source;
use lsr_format::merge::{self, Merge};

/// The project folder `git:<rev>` sides are read from: `--project`, else
/// the first side that is a folder, else the current folder.
fn git_project(project: Option<&Path>, args: &[&str]) -> PathBuf {
    project.map(Path::to_owned).unwrap_or_else(|| {
        args.iter()
            .find(|a| !a.starts_with("git:"))
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
    })
}

fn load(arg: &str, project: &Path) -> Result<lsr_format::Project, String> {
    let source = Source::parse(arg, project);
    source.load().map_err(|e| format!("{source}: {e}"))
}

fn error(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(2)
}

/// `lasair diff <a> <b>`: what changed from `a` to `b`.
pub fn diff(a: &str, b: &str, project: Option<&Path>) -> ExitCode {
    let dir = git_project(project, &[a, b]);
    let (old, new) = match (load(a, &dir), load(b, &dir)) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(e), _) | (_, Err(e)) => return error(e),
    };
    print!("{}", lsr_format::diff::diff(&old, &new));
    ExitCode::SUCCESS
}

/// Prints the conflicts and what was settled by rule.
fn report(m: &Merge) {
    for a in &m.auto {
        println!("auto  {}: {}", a.label, a.note);
    }
    for c in &m.conflicts {
        println!(
            "CONFLICT ({}) {}: {}",
            c.kind.name(),
            c.label,
            c.kind.describe()
        );
        for (side, v) in [("base", &c.base), ("ours", &c.ours), ("theirs", &c.theirs)] {
            let text = merge::value_text(&c.slot, v.as_ref());
            let text = if text.is_empty() {
                "(none)".into()
            } else {
                text
            };
            let first = text.lines().next().unwrap_or("");
            let more = if text.lines().nth(1).is_some() {
                " …"
            } else {
                ""
            };
            println!("    {side:<6} {first}{more}");
        }
    }
}

/// `lasair merge <base> <ours> <theirs> [--out dir]`.
pub fn merge(
    base: &str,
    ours: &str,
    theirs: &str,
    out: Option<&Path>,
    project: Option<&Path>,
) -> ExitCode {
    let dir = git_project(project, &[ours, theirs, base]);
    let sides = [base, ours, theirs].map(|a| load(a, &dir));
    let [b, o, t] = match sides {
        [Ok(b), Ok(o), Ok(t)] => [b, o, t],
        [Err(e), _, _] | [_, Err(e), _] | [_, _, Err(e)] => return error(e),
    };
    let m = merge::merge(&b, &o, &t);
    report(&m);
    if m.is_clean() {
        println!("merged cleanly");
        if let Some(out) = out {
            if let Err(e) = merge::save(out, &m) {
                return error(e);
            }
            println!("wrote {}", out.display());
        }
        return ExitCode::SUCCESS;
    }
    println!("{} conflict(s)", m.conflicts.len());
    if let Some(out) = out {
        if let Err(e) = merge::save_conflicted(out, &m) {
            eprintln!("error: {e}");
            eprintln!("nothing written; run the merge in the app (View > Merge)");
            return ExitCode::FAILURE;
        }
        println!(
            "wrote {} with conflict blocks: open it in the app (View > Merge > Open conflicted)",
            out.display()
        );
    }
    ExitCode::FAILURE
}

/// `lasair merge-file <base> <ours> <theirs> [path]`: the merge driver
/// for one network file (`%O %A %B %P`). Writes the result over `ours`.
pub fn merge_file(base: &Path, ours: &Path, theirs: &Path, path: Option<&str>) -> ExitCode {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let texts = [base, ours, theirs].map(read);
    let [b, o, t] = match texts {
        [Ok(b), Ok(o), Ok(t)] => [b, o, t],
        [Err(e), _, _] | [_, Err(e), _] | [_, _, Err(e)] => return error(e),
    };
    let name = path.unwrap_or("network");
    let m = match merge::merge_network_text(&b, &o, &t) {
        Ok(m) => m,
        // Leave ours as it is: Git reports the file as conflicted.
        Err(e) => return error(format!("{name}: {e}")),
    };
    if let Err(e) = std::fs::write(ours, &m.text) {
        return error(format!("{}: {e}", ours.display()));
    }
    for a in &m.auto {
        eprintln!("lasair: {name}: {}: {}", a.label, a.note);
    }
    if m.conflicts == 0 {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "lasair: {name}: {} conflict(s); resolve in the app (View > Merge > Open conflicted)",
            m.conflicts
        );
        ExitCode::FAILURE
    }
}
