//! Which projects' Python operators the user allowed, by content hash
//! (`docs/python-ops.md`, "Trust"). Kept in the user's settings, never in a
//! project, so a project cannot allow itself.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The settings file name.
const FILE: &str = "allowed-python-ops.txt";

/// A list of allowed `ops/` hashes in a text file: one `<sha256> <project
/// path>` per line (the path is for people reading the file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    /// The store in the user's Lasair settings folder: `%APPDATA%\Lasair`
    /// on Windows, `~/Library/Application Support/Lasair` on macOS,
    /// `$XDG_CONFIG_HOME/lasair` (or `~/.config/lasair`) elsewhere.
    pub fn user() -> Option<Self> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let dir = if cfg!(windows) {
            var("APPDATA")?.join("Lasair")
        } else if cfg!(target_os = "macos") {
            var("HOME")?.join("Library/Application Support/Lasair")
        } else {
            var("XDG_CONFIG_HOME")
                .filter(|p| p.is_absolute())
                .or_else(|| var("HOME").map(|h| h.join(".config")))?
                .join("lasair")
        };
        Some(Self::at(dir.join(FILE)))
    }

    /// A store in the file `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `hash` was allowed. A missing or unreadable file allows
    /// nothing.
    pub fn contains(&self, hash: &str) -> bool {
        if hash.is_empty() {
            return false;
        }
        std::fs::read_to_string(&self.path).is_ok_and(|text| {
            text.lines()
                .filter(|l| !l.starts_with('#'))
                .any(|l| l.split_whitespace().next() == Some(hash))
        })
    }

    /// Remembers `hash` as allowed (the project's `ops/` files exactly as
    /// they are now). Refused in a folder whose app data was removed this
    /// session ([`lsr_core::user_data`]).
    pub fn remember(&self, hash: &str, project: &Path) -> std::io::Result<()> {
        if hash.is_empty() || self.contains(hash) {
            return Ok(());
        }
        if lsr_core::user_data::was_removed(&self.path) {
            return Err(lsr_core::user_data::removed_error(&self.path));
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let new = !self.path.exists();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        if new {
            writeln!(
                f,
                "# Lasair: projects whose Python operators you allowed to run, by the SHA-256 \
                 of their ops/ folder (docs/python-ops.md). Delete a line to forget it."
            )?;
        }
        writeln!(f, "{hash} {}", project.display())
    }
}
