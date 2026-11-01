//! Where model files and licence acceptances live (PRD A61).

use std::path::{Path, PathBuf};

/// The per-user model cache:
///
/// - `LSR_MODELS_DIR`, when set and not empty;
/// - macOS: `~/Library/Caches/Lasair/models`;
/// - Windows: `%LOCALAPPDATA%\Lasair\models`;
/// - Linux and others: `$XDG_CACHE_HOME/lasair/models` (when absolute),
///   else `~/.cache/lasair/models`.
pub fn models_root() -> PathBuf {
    var("LSR_MODELS_DIR").unwrap_or_else(platform_models_root)
}

/// The app's own per-user folder for `accepted.toml`, `hosts.toml` and
/// `secrets.toml`: the folder around the platform's model cache
/// (`~/Library/Caches/Lasair` on macOS), whatever `LSR_MODELS_DIR` says. A
/// folder the user chose for models may have other tools' files of the same
/// names beside it, so the app never reads, writes or removes those.
pub fn settings_dir() -> PathBuf {
    let models = platform_models_root();
    models
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| models.clone(), Path::to_path_buf)
}

fn var(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// [`models_root`] without `LSR_MODELS_DIR`.
fn platform_models_root() -> PathBuf {
    let home = var("HOME").or_else(|| var("USERPROFILE"));
    if cfg!(target_os = "macos") {
        if let Some(home) = home {
            return home.join("Library/Caches/Lasair/models");
        }
    } else if cfg!(windows) {
        if let Some(local) = var("LOCALAPPDATA") {
            return local.join("Lasair").join("models");
        }
        if let Some(home) = home {
            return home
                .join("AppData")
                .join("Local")
                .join("Lasair")
                .join("models");
        }
    } else {
        if let Some(xdg) = var("XDG_CACHE_HOME").filter(|p| p.is_absolute()) {
            return xdg.join("lasair/models");
        }
        if let Some(home) = home {
            return home.join(".cache/lasair/models");
        }
    }
    std::env::temp_dir().join("lasair").join("models")
}

/// A model cache folder and the folder of its acceptance file, host tokens
/// and cloud keys. Tests make their own; the app and CLI use
/// [`Store::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
    accepted: PathBuf,
}

impl Default for Store {
    /// [`models_root`], with the settings files in [`settings_dir`].
    fn default() -> Self {
        Self::with_settings_dir(models_root(), settings_dir())
    }
}

impl Store {
    /// Models in `root/<id>/`; acceptances in `accepted.toml` beside `root`
    /// (in its parent folder), so clearing the models keeps the record. For
    /// a folder the app owns, as tests make; the user's own takes
    /// [`Store::with_settings_dir`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let dir = root
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| root.clone(), Path::to_path_buf);
        Self::with_settings_dir(root, dir)
    }

    /// Models in `root/<id>/`; `accepted.toml`, `hosts.toml` and
    /// `secrets.toml` in `settings`.
    pub fn with_settings_dir(root: impl Into<PathBuf>, settings: impl AsRef<Path>) -> Self {
        Self {
            root: root.into(),
            accepted: settings.as_ref().join("accepted.toml"),
        }
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

    /// `hosts.toml`: LAN and cloud host tokens, beside `accepted.toml`
    /// (`crate::hosts`).
    pub fn hosts_path(&self) -> PathBuf {
        self.accepted.with_file_name("hosts.toml")
    }

    /// `secrets.toml`: cloud keys, beside `accepted.toml`
    /// ([`crate::cloud::secrets_path`]).
    pub fn secrets_path(&self) -> PathBuf {
        self.accepted.with_file_name("secrets.toml")
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
