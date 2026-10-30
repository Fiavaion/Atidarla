//! Where model files and licence acceptances live (PRD A61).

use std::path::{Path, PathBuf};

/// The per-user model cache:
///
/// - `ATD_MODELS_DIR`, when set and not empty;
/// - macOS: `~/Library/Caches/Atidarla/models`;
/// - Windows: `%LOCALAPPDATA%\Atidarla\models`;
/// - Linux and others: `$XDG_CACHE_HOME/atidarla/models` (when absolute),
///   else `~/.cache/atidarla/models`.
pub fn models_root() -> PathBuf {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var("ATD_MODELS_DIR") {
        return dir;
    }
    let home = var("HOME").or_else(|| var("USERPROFILE"));
    if cfg!(target_os = "macos") {
        if let Some(home) = home {
            return home.join("Library/Caches/Atidarla/models");
        }
    } else if cfg!(windows) {
        if let Some(local) = var("LOCALAPPDATA") {
            return local.join("Atidarla").join("models");
        }
        if let Some(home) = home {
            return home
                .join("AppData")
                .join("Local")
                .join("Atidarla")
                .join("models");
        }
    } else {
        if let Some(xdg) = var("XDG_CACHE_HOME").filter(|p| p.is_absolute()) {
            return xdg.join("atidarla/models");
        }
        if let Some(home) = home {
            return home.join(".cache/atidarla/models");
        }
    }
    std::env::temp_dir().join("atidarla").join("models")
}

/// A model cache folder and its acceptance file. Tests make their own; the
/// app and CLI use [`Store::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
    accepted: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::new(models_root())
    }
}

impl Store {
    /// Models in `root/<id>/`; acceptances in `accepted.toml` beside `root`
    /// (in its parent folder), so clearing the models keeps the record.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let accepted = root
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| root.join("accepted.toml"), |p| p.join("accepted.toml"));
        Self { root, accepted }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn model_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// `accepted.toml`.
    pub fn accepted_path(&self) -> &Path {
        &self.accepted
    }

    /// Where `rel` (a catalogue path, `/`-separated) of model `id` lives.
    pub fn file(&self, id: &str, rel: &str) -> PathBuf {
        let mut p = self.model_dir(id);
        p.extend(rel.split('/'));
        p
    }

    /// The partial download of `rel`.
    pub fn part(&self, id: &str, rel: &str) -> PathBuf {
        let mut p = self.file(id, rel).into_os_string();
        p.push(".part");
        PathBuf::from(p)
    }
}
