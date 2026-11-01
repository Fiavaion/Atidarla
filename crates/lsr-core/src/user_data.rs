//! The per-user folders whose app data was removed this session (PRD A70 c:
//! the Storage panel's Remove and Remove app data…, `lasair storage`).
//!
//! Every writer of a per-user settings file (`ui.toml`, `learn.toml`, the
//! trusted Python operators, host tokens, cloud keys) asks [`was_removed`]
//! first and writes nothing into a removed folder, so a removal is not undone
//! by the next zoom change or a background save. Folders, not one flag, so
//! that tests removing scratch folders never stop each other's writes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static REMOVED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Records that the app's data in `dir` was removed: nothing more is
/// written in it until the process ends.
pub fn mark_removed(dir: &Path) {
    let mut removed = REMOVED.lock().unwrap_or_else(|e| e.into_inner());
    if !removed.iter().any(|d| d == dir) {
        removed.push(dir.to_path_buf());
    }
}

/// Whether `path` is in a folder whose app data was removed this session.
pub fn was_removed(path: &Path) -> bool {
    REMOVED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|d| path.starts_with(d))
}

/// The error a writer returns for a file in a removed folder.
pub fn removed_error(path: &Path) -> std::io::Error {
    std::io::Error::other(format!(
        "{}: Lasair's app data was removed this session, so nothing is written there until \
         the app restarts",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_in_a_removed_folder_are_refused() {
        let dir = std::env::temp_dir().join(format!("lsr-user-data-{}", std::process::id()));
        assert!(!was_removed(&dir.join("ui.toml")));
        mark_removed(&dir);
        mark_removed(&dir);
        assert!(was_removed(&dir.join("ui.toml")));
        assert!(was_removed(&dir));
        assert!(!was_removed(
            &dir.with_file_name("elsewhere").join("ui.toml")
        ));
        assert!(
            removed_error(&dir)
                .to_string()
                .contains("removed this session")
        );
    }
}
