//! Per-user interface settings (not in any project), kept in `ui.toml` in
//! the app's config folder: the UI scale (Ctrl/Cmd +, -, 0) and whether
//! wires carry their type as a label. See docs/accessibility.md.

use std::path::PathBuf;

use toml_edit::{DocumentMut, value};

const FILE: &str = "ui.toml";
/// The UI scale range, as egui's keyboard zoom allows.
pub const MIN_SCALE: f32 = 0.2;
pub const MAX_SCALE: f32 = 5.0;

/// The app's per-user config folder: `%APPDATA%\Atidarla` on Windows,
/// `~/Library/Application Support/Atidarla` on macOS, else
/// `$XDG_CONFIG_HOME/atidarla` (or `~/.config/atidarla`).
pub fn config_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        Some(var("APPDATA")?.join("Atidarla"))
    } else if cfg!(target_os = "macos") {
        Some(var("HOME")?.join("Library/Application Support/Atidarla"))
    } else {
        Some(
            var("XDG_CONFIG_HOME")
                .or_else(|| var("HOME").map(|h| h.join(".config")))?
                .join("atidarla"),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiSettings {
    /// egui's zoom factor: 1.0 is 100%.
    pub scale: f32,
    /// Draw each wire's type (IMG, SIG, ...) at its middle.
    pub wire_labels: bool,
    /// Where they are kept; `None` keeps them in memory (tests, smoke runs).
    path: Option<PathBuf>,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            wire_labels: false,
            path: None,
        }
    }
}

impl UiSettings {
    /// The settings in `path`; a missing or damaged file gives the defaults.
    pub fn load(path: PathBuf) -> Self {
        let mut s = Self {
            path: Some(path.clone()),
            ..Self::default()
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return s;
        };
        match text.parse::<DocumentMut>() {
            Ok(doc) => {
                if let Some(v) = doc.get("scale").and_then(|v| v.as_float()) {
                    s.scale = clamp_scale(v as f32);
                }
                if let Some(v) = doc.get("wire_labels").and_then(|v| v.as_bool()) {
                    s.wire_labels = v;
                }
            }
            Err(e) => log::warn!("settings: {}: {e}; using the defaults", path.display()),
        }
        s
    }

    /// The user's settings, or in-memory defaults without a config folder.
    pub fn user() -> Self {
        config_dir().map_or_else(Self::default, |d| Self::load(d.join(FILE)))
    }

    /// The file's text.
    pub fn to_toml(&self) -> String {
        let mut doc = DocumentMut::new();
        doc["scale"] = value(f64::from((self.scale * 100.0).round() / 100.0));
        doc["wire_labels"] = value(self.wire_labels);
        doc.to_string()
    }

    /// Writes the settings on a worker thread (the UI thread never waits
    /// for the disk). Does nothing for in-memory settings.
    pub fn save(&self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let text = self.to_toml();
        std::thread::spawn(move || {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, text));
            if let Err(e) = written {
                log::warn!("settings: could not write {}: {e}", path.display());
            }
        });
    }

    /// Follows the context's zoom factor (changed by Ctrl/Cmd +, - and 0
    /// or the View menu); saves when it changed. Returns whether it did.
    pub fn follow_zoom(&mut self, zoom: f32) -> bool {
        let zoom = clamp_scale(zoom);
        if (zoom - self.scale).abs() < 1e-4 {
            return false;
        }
        self.scale = zoom;
        self.save();
        true
    }

    /// Stops writing to disk (smoke runs leave the user's settings alone).
    pub fn in_memory(mut self) -> Self {
        self.path = None;
        self
    }
}

pub fn clamp_scale(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atd-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(FILE)
    }

    #[test]
    fn scale_and_labels_round_trip_through_the_file() {
        let path = temp("round-trip");
        let mut s = UiSettings::load(path.clone());
        assert_eq!(s.scale, 1.0);
        s.wire_labels = true;
        assert!(s.follow_zoom(1.5));
        assert!(!s.follow_zoom(1.5), "unchanged zoom is not saved again");
        // The write runs on a thread.
        for _ in 0..100 {
            if UiSettings::load(path.clone()).scale == 1.5 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let back = UiSettings::load(path.clone());
        assert_eq!(back.scale, 1.5);
        assert!(back.wire_labels);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn damaged_or_out_of_range_files_give_safe_values() {
        let path = temp("damaged");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "scale = 40.0\nwire_labels = \"yes\"\n").unwrap();
        let s = UiSettings::load(path.clone());
        assert_eq!(s.scale, MAX_SCALE);
        assert!(!s.wire_labels);
        std::fs::write(&path, "not toml [[[").unwrap();
        assert_eq!(UiSettings::load(path.clone()).scale, 1.0);
        assert_eq!(clamp_scale(f32::NAN), 1.0);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn in_memory_settings_never_write() {
        let path = temp("memory");
        let mut s = UiSettings::load(path.clone()).in_memory();
        assert!(s.follow_zoom(2.0));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!path.exists());
    }
}
