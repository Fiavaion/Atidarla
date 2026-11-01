//! A project as it was at a Git revision, for diff and merge (FR-F2,
//! FR-F3). Runs the `git` on PATH; this blocks, so callers on a UI thread
//! run it on a worker.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{FormatError, Project};

/// The files a project's model is read from; media and caches are skipped.
fn is_project_file(path: &str) -> bool {
    path == "project.toml"
        || path == crate::mappings::FILE
        || path == crate::timeline::FILE
        || path.starts_with("networks/")
        || path.starts_with("packs/")
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, FormatError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| FormatError::Invalid(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(FormatError::Invalid(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// Reads the project folder `dir` as it was at `rev` (any revision Git
/// understands: `HEAD~1`, a branch, a commit).
pub fn load_at_rev(dir: &Path, rev: &str) -> Result<Project, FormatError> {
    if rev.is_empty() || rev.starts_with('-') {
        return Err(FormatError::Invalid(format!("`{rev}` is not a revision")));
    }
    // Paths come out relative to `dir`.
    let list = git(dir, &["ls-tree", "-r", "-z", "--name-only", rev, "--", "."])?;
    let tmp = scratch_dir();
    let result = (|| {
        for name in list.split(|b| *b == 0).filter(|n| !n.is_empty()) {
            let name = String::from_utf8_lossy(name).into_owned();
            if !is_project_file(&name) {
                continue;
            }
            let bytes = git(dir, &["show", &format!("{rev}:./{name}")])?;
            let path = tmp.join(&name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| FormatError::Io {
                    path: parent.to_owned(),
                    source,
                })?;
            }
            std::fs::write(&path, bytes).map_err(|source| FormatError::Io {
                path: path.clone(),
                source,
            })?;
        }
        crate::load(&tmp)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "lasair-git-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Where one side of a diff or merge comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A project folder.
    Folder(PathBuf),
    /// `git:<rev>`: a project folder at a Git revision.
    Git { dir: PathBuf, rev: String },
}

impl Source {
    /// `git:<rev>` (read from `project`), otherwise a folder.
    pub fn parse(arg: &str, project: &Path) -> Self {
        match arg.strip_prefix("git:") {
            Some(rev) => Self::Git {
                dir: project.to_owned(),
                rev: rev.to_owned(),
            },
            None => Self::Folder(PathBuf::from(arg)),
        }
    }

    pub fn load(&self) -> Result<Project, FormatError> {
        match self {
            Self::Folder(dir) => crate::load(dir),
            Self::Git { dir, rev } => load_at_rev(dir, rev),
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Folder(dir) => write!(f, "{}", dir.display()),
            Self::Git { dir, rev } => write!(f, "{} at {rev}", dir.display()),
        }
    }
}
