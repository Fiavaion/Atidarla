//! A student's progress through the quests, kept per user (not in any
//! project) in the app's config folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, value};

use super::check::Snapshot;
use super::quest::Quest;

const FILE: &str = "learn.toml";

/// Steps finished per quest id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    steps: BTreeMap<String, usize>,
    path: Option<PathBuf>,
}

impl Progress {
    /// Progress kept in `path`. A missing or damaged file starts afresh.
    pub fn load(path: PathBuf) -> Self {
        let mut p = Self {
            steps: BTreeMap::new(),
            path: Some(path.clone()),
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return p;
        };
        match text.parse::<DocumentMut>() {
            Ok(doc) => {
                if let Some(t) = doc.get("steps").and_then(Item::as_table_like) {
                    for (k, v) in t.iter() {
                        if let Some(n) = v.as_integer().and_then(|n| usize::try_from(n).ok()) {
                            p.steps.insert(k.to_owned(), n);
                        }
                    }
                }
            }
            Err(e) => log::warn!("learn: {}: {e}; starting afresh", path.display()),
        }
        p
    }

    /// Progress that is never written (tests, no config folder).
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The per-user file: next to the other per-user settings.
    pub fn user_file() -> Option<PathBuf> {
        Some(crate::settings::config_dir()?.join(FILE))
    }

    /// Steps finished in quest `id`.
    pub fn done(&self, id: &str) -> usize {
        self.steps.get(id).copied().unwrap_or(0)
    }

    pub fn is_complete(&self, quest: &Quest) -> bool {
        self.done(&quest.id) >= quest.steps.len()
    }

    /// Records `n` finished steps; returns whether that changed anything.
    pub fn set(&mut self, id: &str, n: usize) -> bool {
        if n == 0 {
            return self.steps.remove(id).is_some();
        }
        self.steps.insert(id.to_owned(), n) != Some(n)
    }

    /// Writes nothing in memory, or in a folder whose app data was removed
    /// this session ([`lsr_core::user_data`]).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if lsr_core::user_data::was_removed(path) {
            return Ok(());
        }
        let mut doc = DocumentMut::new();
        doc.decor_mut()
            .set_prefix("# Lasair Learn progress: steps finished per quest.\n");
        doc["format"] = value(1);
        let mut table = toml_edit::Table::new();
        for (k, n) in &self.steps {
            table[k.as_str()] = value(*n as i64);
        }
        doc["steps"] = Item::Table(table);
        write_atomic(path, &doc.to_string())
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Moves `done` forward over every step whose check now holds, in order.
/// Returns whether it moved. A step that was done stays done: progress is
/// not taken back when the student later changes the patch.
pub fn advance(quest: &Quest, done: &mut usize, snapshot: &Snapshot<'_>) -> bool {
    let before = *done;
    while let Some(step) = quest.steps.get(*done) {
        if !step.check.eval(snapshot) {
            break;
        }
        *done += 1;
    }
    *done != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_round_trips_and_survives_damage() {
        let dir = std::env::temp_dir().join(format!("lsr-learn-{}", std::process::id()));
        let file = dir.join("learn.toml");
        let mut p = Progress::load(file.clone());
        assert_eq!(p.done("01-first-patch"), 0);
        assert!(p.set("01-first-patch", 3));
        assert!(!p.set("01-first-patch", 3));
        p.set("02-audio-reactive", 5);
        p.save().unwrap();
        let q = Progress::load(file.clone());
        assert_eq!(q, p);
        assert_eq!(q.done("02-audio-reactive"), 5);
        std::fs::write(&file, "steps = [[[ not toml").unwrap();
        assert_eq!(Progress::load(file).done("01-first-patch"), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_written_after_app_data_is_removed() {
        let dir = std::env::temp_dir().join(format!("lsr-learn-removed-{}", std::process::id()));
        let file = dir.join("learn.toml");
        let mut p = Progress::load(file.clone());
        p.set("01-first-patch", 1);
        lsr_core::user_data::mark_removed(&dir);
        p.save().unwrap();
        assert!(!file.exists());
    }
}
