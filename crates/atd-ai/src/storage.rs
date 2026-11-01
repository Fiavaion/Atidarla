//! Everything the app keeps per user, outside projects, and removing it
//! (PRD A70 c): `atidarla storage`, the MSI uninstall and the Storage panel.
//!
//! | Kind       | What                                  | Where (macOS; Windows; Linux)                              |
//! |------------|---------------------------------------|------------------------------------------------------------|
//! | `packages` | AI Host extras ([`hostenv`])          | `~/Library/Application Support/Atidarla/ai-host`; `%LOCALAPPDATA%\Atidarla\ai-host`; `~/.local/share/atidarla/ai-host` |
//! | `models`   | model files and licence acceptances   | `~/Library/Caches/Atidarla/{models,accepted.toml}`; `%LOCALAPPDATA%\Atidarla\...`; `~/.cache/atidarla/...` |
//! | `settings` | host tokens, cloud keys               | `hosts.toml`, `secrets.toml` beside `accepted.toml`         |
//! | `settings` | interface, Learn progress, trusted Python operators | `ui.toml`, `learn.toml`, `allowed-python-ops.txt` in `~/Library/Application Support/Atidarla`; `%APPDATA%\Atidarla`; `~/.config/atidarla` |
//! | `cache`    | caches and logs                       | none today: logs go to stderr and caches stay in memory or in a project's `.cache/` |
//!
//! Only these paths are removed, never the folders around them unless they
//! are left empty. Models that other tools manage (Ollama's `~/.ollama`,
//! Hugging Face's global cache) are not on the list. When `ATD_MODELS_DIR`
//! points the model cache at a folder of the user's choosing, only the
//! catalogue models' own folders in it are listed, and nothing beside it:
//! `accepted.toml`, `hosts.toml` and `secrets.toml` stay in the app's own
//! folder ([`models::settings_dir`]) whatever the variable says. Symbolic links are
//! removed as links: neither sizing nor removing follows one out of the
//! folder.
//!
//! Sizing walks the folders and removing deletes them, so both block: call
//! them from a command or a worker thread ([`start_list`]), never the
//! render thread.

use std::path::{Path, PathBuf};

use crate::hostenv;
use crate::models::{self, Job, Store};

/// What a stored item is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// AI Host packages (optional extras).
    Packages,
    /// Model files and licence acceptances: kept unless asked, since a
    /// reinstall or another tool pointed at the folder may reuse them.
    Models,
    /// Caches and logs.
    Cache,
    /// Settings, host tokens and cloud keys.
    Settings,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Packages, Kind::Models, Kind::Cache, Kind::Settings];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Packages => "packages",
            Kind::Models => "models",
            Kind::Cache => "cache",
            Kind::Settings => "settings",
        }
    }
}

impl std::str::FromStr for Kind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Kind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| format!("unknown kind `{s}` (packages, models, cache, settings)"))
    }
}

/// One stored item that exists, with its size on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    /// What it holds, for people ("AI Host packages", "host tokens").
    pub what: String,
    /// A folder or a file.
    pub path: PathBuf,
    /// Bytes, symbolic links not followed.
    pub size: u64,
}

/// Why something could not be removed.
#[derive(Debug, thiserror::Error)]
#[error("could not remove {}: {source}", path.display())]
pub struct RemoveError {
    pub path: PathBuf,
    pub source: std::io::Error,
}

/// What a removal did: the entries removed and those that failed (the rest
/// are still removed when one fails).
#[derive(Debug, Default)]
pub struct Removal {
    pub removed: Vec<Entry>,
    pub failed: Vec<RemoveError>,
}

impl Removal {
    /// Bytes freed.
    pub fn freed(&self) -> u64 {
        self.removed.iter().map(|e| e.size).sum()
    }
}

/// The folders the app writes per user. [`Storage::user`] is this user's;
/// tests build their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    /// AI Host packages ([`hostenv::data_root`]).
    pub packages: PathBuf,
    /// The model cache ([`models::models_root`]).
    pub models: PathBuf,
    /// The model cache is a folder the user chose (`ATD_MODELS_DIR`): only
    /// catalogue models' folders in it are the app's, and nothing around it.
    pub models_chosen: bool,
    /// The app's folder of acceptances, host tokens and cloud keys
    /// ([`models::settings_dir`]), never the parent of a chosen model folder.
    pub settings: PathBuf,
    /// The settings folder (`ui.toml` and others).
    pub config: Option<PathBuf>,
}

/// Files in the settings folder, as their owners name them
/// (`atd-app::settings`, `atd-app::learn::progress`, `atd-py::ops::trust`).
const CONFIG_FILES: [(&str, &str); 3] = [
    ("ui.toml", "interface settings"),
    ("learn.toml", "Learn progress"),
    ("allowed-python-ops.txt", "trusted Python operators"),
];

impl Storage {
    /// This user's folders.
    pub fn user() -> Self {
        Self {
            packages: hostenv::data_root(),
            models: models::models_root(),
            models_chosen: std::env::var_os("ATD_MODELS_DIR").is_some_and(|v| !v.is_empty()),
            settings: models::settings_dir(),
            config: config_dir(),
        }
    }

    /// Every item the app may have written, existing or not.
    fn items(&self) -> Vec<(Kind, String, PathBuf)> {
        let store = Store::with_settings_dir(&self.models, &self.settings);
        let mut out = vec![(
            Kind::Packages,
            "AI Host packages".to_owned(),
            self.packages.clone(),
        )];
        if self.models_chosen {
            for m in models::catalogue().iter() {
                out.push((
                    Kind::Models,
                    format!("model {}", m.id),
                    store.model_dir(&m.id),
                ));
            }
        } else {
            out.push((Kind::Models, "model files".to_owned(), self.models.clone()));
        }
        out.push((
            Kind::Models,
            "licence acceptances".to_owned(),
            store.accepted_path().to_path_buf(),
        ));
        out.push((Kind::Settings, "host tokens".to_owned(), store.hosts_path()));
        out.push((
            Kind::Settings,
            "cloud keys".to_owned(),
            store.secrets_path(),
        ));
        if let Some(config) = &self.config {
            for (file, what) in CONFIG_FILES {
                out.push((Kind::Settings, what.to_owned(), config.join(file)));
            }
        }
        out
    }

    /// The items that exist, with their sizes. Walks every folder: blocks.
    pub fn list(&self) -> Vec<Entry> {
        self.items()
            .into_iter()
            .filter(|(_, _, path)| path.symlink_metadata().is_ok())
            .map(|(kind, what, path)| Entry {
                kind,
                what,
                size: size_of(&path),
                path,
            })
            .collect()
    }

    /// Removes the items of `kinds`, then the app's folders left empty.
    pub fn remove(&self, kinds: &[Kind]) -> Removal {
        let mut out = Removal::default();
        for entry in self.list() {
            if !kinds.contains(&entry.kind) {
                continue;
            }
            match remove_path(&entry.path) {
                Ok(()) => out.removed.push(entry),
                Err(source) => out.failed.push(RemoveError {
                    path: entry.path,
                    source,
                }),
            }
        }
        self.prune();
        out
    }

    /// Removes everything but the model files and their acceptances (the
    /// first half of an uninstall; the models are the user's choice).
    pub fn remove_everything_but_models(&self) -> Removal {
        self.remove(&[Kind::Packages, Kind::Cache, Kind::Settings])
    }

    /// The app's own folders, removed when empty (never forced).
    fn prune(&self) {
        let mut dirs = vec![];
        if let Some(parent) = self.packages.parent() {
            dirs.push(parent.to_path_buf());
        }
        // A chosen model folder and the one around it are the user's.
        if !self.models_chosen {
            dirs.push(self.models.clone());
        }
        dirs.push(self.settings.clone());
        dirs.extend(self.config.clone());
        for dir in dirs {
            // Fails, as it should, when the folder holds anything else.
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// [`Storage::list`] on a worker thread, for a UI to poll.
pub fn start_list(storage: Storage) -> Job<Vec<Entry>> {
    Job::spawn("atd-storage-list", move |_| storage.list())
}

/// The settings folder: `%APPDATA%\Atidarla` on Windows,
/// `~/Library/Application Support/Atidarla` on macOS, else
/// `$XDG_CONFIG_HOME/atidarla` (or `~/.config/atidarla`). The app keeps
/// `ui.toml` and `learn.toml` here (`atd-app::settings::config_dir` is this).
pub fn config_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        // An MSI custom action may run without APPDATA set.
        var("APPDATA")
            .or_else(|| var("USERPROFILE").map(|h| h.join("AppData").join("Roaming")))
            .map(|d| d.join("Atidarla"))
    } else if cfg!(target_os = "macos") {
        Some(var("HOME")?.join("Library/Application Support/Atidarla"))
    } else {
        xdg_config_dir(var)
    }
}

/// `$XDG_CONFIG_HOME/atidarla` when that is absolute (the XDG spec says to
/// ignore a relative one), else `~/.config/atidarla`.
fn xdg_config_dir(var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    Some(
        var("XDG_CONFIG_HOME")
            .filter(|p| p.is_absolute())
            .or_else(|| var("HOME").map(|h| h.join(".config")))?
            .join("atidarla"),
    )
}

/// Bytes for people, in decimal units as file managers show them.
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", b / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", b / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// Why the model files are kept unless asked (PRD A70 c), for the
/// uninstall's question and the Storage panel: their size, their folder and
/// who may reuse them. `kept` is the [`Kind::Models`] entries.
pub fn models_explanation(kept: &[Entry]) -> String {
    let size: u64 = kept.iter().map(|e| e.size).sum();
    let folder = kept
        .iter()
        .find(|e| e.what == "model files")
        .or(kept.first())
        .map(|e| e.path.display().to_string())
        .unwrap_or_default();
    format!(
        "The model files ({}, in {folder}) may be reused: a reinstall of Atidarla, or other \
         tools pointed at that folder, can use them without downloading them again. Models \
         that other tools manage themselves, such as Ollama's own store, are never touched.",
        human_size(size)
    )
}

/// Bytes under `path`, counting a symbolic link as itself and never what it
/// points to.
fn size_of(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = p.symlink_metadata() else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(dir) = std::fs::read_dir(&p) {
                stack.extend(dir.flatten().map(|e| e.path()));
            }
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

/// Removes a file, a folder and what is in it, or a symbolic link (itself,
/// not its target). `remove_dir_all` does not follow links inside.
fn remove_path(path: &Path) -> std::io::Result<()> {
    let meta = path.symlink_metadata()?;
    if meta.is_symlink() {
        // A link to a folder is a directory entry on Windows.
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
    } else if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atd-storage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    /// The layout a user has on macOS, under `root`, with something in
    /// every item and a file that is not the app's in each folder.
    fn layout(root: &Path) -> Storage {
        let s = Storage {
            packages: root.join("data/Atidarla/ai-host"),
            models: root.join("cache/Atidarla/models"),
            models_chosen: false,
            settings: root.join("cache/Atidarla"),
            config: Some(root.join("data/Atidarla")),
        };
        write(&s.packages.join("python3.13/site-packages/a.py"), 100);
        write(&s.packages.join("python3.13/extras.txt"), 7);
        write(&s.models.join("sd-turbo/unet/x.safetensors"), 1000);
        let cache = root.join("cache/Atidarla");
        write(&cache.join("accepted.toml"), 10);
        write(&cache.join("hosts.toml"), 20);
        write(&cache.join("secrets.toml"), 30);
        write(&root.join("data/Atidarla/ui.toml"), 40);
        write(&root.join("cache/ollama/model.gguf"), 5);
        s
    }

    fn kinds(entries: &[Entry]) -> Vec<(Kind, &str, u64)> {
        entries
            .iter()
            .map(|e| (e.kind, e.what.as_str(), e.size))
            .collect()
    }

    #[test]
    fn lists_what_exists_with_sizes() {
        let root = scratch("list");
        let s = layout(&root);
        assert_eq!(
            kinds(&s.list()),
            [
                (Kind::Packages, "AI Host packages", 107),
                (Kind::Models, "model files", 1000),
                (Kind::Models, "licence acceptances", 10),
                (Kind::Settings, "host tokens", 20),
                (Kind::Settings, "cloud keys", 30),
                (Kind::Settings, "interface settings", 40),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn uninstall_keeps_models_and_what_is_not_the_apps() {
        let root = scratch("uninstall");
        let s = layout(&root);
        let r = s.remove_everything_but_models();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.freed(), 107 + 20 + 30 + 40);
        assert_eq!(
            kinds(&s.list()),
            [
                (Kind::Models, "model files", 1000),
                (Kind::Models, "licence acceptances", 10),
            ]
        );
        // The data folder is empty, so it goes; the cache folder keeps the
        // models.
        assert!(!root.join("data/Atidarla").exists());
        assert!(root.join("cache/ollama/model.gguf").is_file());

        let r = s.remove(&[Kind::Models]);
        assert_eq!(r.freed(), 1010);
        assert!(s.list().is_empty());
        assert!(!root.join("cache/Atidarla").exists());
        assert!(root.join("cache/ollama/model.gguf").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folders_holding_other_files_stay() {
        let root = scratch("others");
        let s = layout(&root);
        write(&root.join("data/Atidarla/notes.txt"), 1);
        s.remove(&Kind::ALL);
        assert!(root.join("data/Atidarla/notes.txt").is_file());
        assert!(!s.packages.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_chosen_model_folder_loses_only_catalogue_models() {
        let root = scratch("chosen");
        let shared = root.join("shared");
        let id = &models::catalogue().iter().next().unwrap().id;
        write(&shared.join(id).join("weights.bin"), 50);
        write(&shared.join("someone-elses/weights.bin"), 60);
        let s = Storage {
            packages: root.join("ai-host"),
            models: shared.clone(),
            models_chosen: true,
            settings: root.join("app"),
            config: None,
        };
        assert_eq!(
            kinds(&s.list()),
            [(Kind::Models, format!("model {id}").as_str(), 50)]
        );
        s.remove(&[Kind::Models]);
        assert!(!shared.join(id).exists());
        assert!(shared.join("someone-elses/weights.bin").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Another tool's settings files of the same names beside a chosen model
    /// folder are not the app's: never listed, never removed. The app's own
    /// are in its settings folder.
    #[test]
    fn files_beside_a_chosen_model_folder_are_never_touched() {
        let root = scratch("beside");
        let ai = root.join("AI");
        let foreign = ["secrets.toml", "hosts.toml", "accepted.toml"];
        for f in foreign {
            write(&ai.join(f), 3);
        }
        let s = Storage {
            packages: root.join("app/ai-host"),
            models: ai.join("models"),
            models_chosen: true,
            settings: root.join("app"),
            config: None,
        };
        write(&s.settings.join("hosts.toml"), 20);
        assert_eq!(kinds(&s.list()), [(Kind::Settings, "host tokens", 20)]);
        assert_eq!(s.list()[0].path, root.join("app/hosts.toml"));

        let r = s.remove_everything_but_models();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        let r = s.remove(&Kind::ALL);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        for f in foreign {
            assert!(
                ai.join(f).is_file(),
                "{f} beside the chosen folder survives"
            );
        }
        assert!(!root.join("app").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The default store keeps its settings files in the app's folder, not
    /// beside `ATD_MODELS_DIR`.
    #[test]
    fn the_default_store_keeps_settings_in_the_apps_folder() {
        let store = Store::default();
        let dir = models::settings_dir();
        assert_eq!(store.accepted_path(), dir.join("accepted.toml"));
        assert_eq!(store.hosts_path(), dir.join("hosts.toml"));
        assert_eq!(store.secrets_path(), dir.join("secrets.toml"));
        assert_eq!(crate::cloud::secrets_path(), dir.join("secrets.toml"));
        assert_eq!(Storage::user().settings, dir);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_never_followed() {
        use std::os::unix::fs::symlink;
        let root = scratch("links");
        let outside = root.join("outside");
        write(&outside.join("keep.bin"), 500);
        let s = layout(&root);
        // A link inside a folder, and an item that is itself a link.
        symlink(&outside, s.packages.join("python3.13/link")).unwrap();
        std::fs::remove_dir_all(&s.models).unwrap();
        symlink(&outside, &s.models).unwrap();

        let list = s.list();
        assert_eq!(list[0].size, 107, "the link inside is not counted");
        assert_eq!(list[1].size, 0, "the linked model folder is not walked");

        let r = s.remove(&Kind::ALL);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert!(s.models.symlink_metadata().is_err(), "the link itself goes");
        assert!(outside.join("keep.bin").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Junctions (what `mklink /J` makes, and what a user without the right
    /// to make symbolic links uses) are links too: never followed.
    #[cfg(windows)]
    #[test]
    fn junctions_are_never_followed() {
        let junction = |link: &Path, target: &Path| {
            let out = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        let root = scratch("junctions");
        let outside = root.join("outside");
        write(&outside.join("keep.bin"), 500);
        let s = layout(&root);
        junction(&s.packages.join("python3.13/link"), &outside);
        std::fs::remove_dir_all(&s.models).unwrap();
        junction(&s.models, &outside);

        let list = s.list();
        assert_eq!(list[0].size, 107, "the junction inside is not counted");
        assert_eq!(list[1].size, 0, "the junction model folder is not walked");

        let r = s.remove(&Kind::ALL);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert!(
            s.models.symlink_metadata().is_err(),
            "the junction itself goes"
        );
        assert!(!s.packages.exists());
        assert!(outside.join("keep.bin").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A relative `XDG_CONFIG_HOME` would point the settings (and their
    /// removal) at a folder under whatever the current directory is.
    #[test]
    fn a_relative_xdg_config_home_is_ignored() {
        let home = std::env::temp_dir().join("home");
        let env = |xdg: &'static str| {
            let home = home.clone();
            move |k: &str| match k {
                "XDG_CONFIG_HOME" => Some(PathBuf::from(xdg)),
                "HOME" => Some(home.clone()),
                _ => None,
            }
        };
        let abs = std::env::temp_dir().join("xdg");
        let abs_env = {
            let abs = abs.clone();
            move |k: &str| (k == "XDG_CONFIG_HOME").then(|| abs.clone())
        };
        assert_eq!(xdg_config_dir(abs_env), Some(abs.join("atidarla")));
        assert_eq!(
            xdg_config_dir(env("relative/config")),
            Some(home.join(".config/atidarla"))
        );
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human_size(40), "40 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(2_340_000), "2.3 MB");
        assert_eq!(human_size(4_200_000_000), "4.20 GB");
    }

    #[test]
    fn the_models_explanation_names_the_folder_size_and_reuse() {
        let kept = [
            Entry {
                kind: Kind::Models,
                what: "licence acceptances".into(),
                path: "/c/Atidarla/accepted.toml".into(),
                size: 0,
            },
            Entry {
                kind: Kind::Models,
                what: "model files".into(),
                path: "/c/Atidarla/models".into(),
                size: 2_000_000_000,
            },
        ];
        let text = models_explanation(&kept);
        assert!(text.contains("2.00 GB, in /c/Atidarla/models"), "{text}");
        assert!(
            text.contains("reinstall") && text.contains("other tools"),
            "{text}"
        );
        assert!(text.contains("never touched"), "{text}");
    }

    #[test]
    fn kinds_parse() {
        for k in Kind::ALL {
            assert_eq!(k.as_str().parse::<Kind>(), Ok(k));
        }
        assert!("weights".parse::<Kind>().is_err());
    }
}
