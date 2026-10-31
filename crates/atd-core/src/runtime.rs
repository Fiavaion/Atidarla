//! The Python runtime an installer bundles beside the executables (PRD A68).
//!
//! The DMG, MSI and AppImage ship a relocatable CPython
//! (python-build-standalone) at a fixed place relative to the program:
//!
//! | Platform | Program | Runtime home |
//! |---|---|---|
//! | macOS | `Atidarla.app/Contents/MacOS/` | `Contents/Resources/python` |
//! | Windows | `<install folder>\` | `<install folder>\python` |
//! | Linux (AppImage) | `usr/bin/` | `usr/lib/atidarla/python` |
//!
//! A development build has none of these, so [`bundled_python_home`] is
//! `None` and the system or Homebrew Python it was linked against is used as
//! before.

use std::path::{Path, PathBuf};

/// The runtime home relative to the folder holding the executable.
const HOME_FROM_EXE_DIR: &str = if cfg!(target_os = "macos") {
    "../Resources/python"
} else if cfg!(windows) {
    "python"
} else {
    "../lib/atidarla/python"
};

/// The bundled runtime's home (its `sys.prefix`) for an executable at
/// `exe`, if the installer put one there.
pub fn bundled_python_home_for(exe: &Path) -> Option<PathBuf> {
    let home = exe.parent()?.join(HOME_FROM_EXE_DIR);
    has_stdlib(&home).then(|| normalise(&home))
}

/// The bundled runtime's home for this process, if there is one.
pub fn bundled_python_home() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // A symlink on the PATH (`ln -s .../Contents/MacOS/atidarla`) points
    // into the bundle: resolve it to find the runtime beside the target.
    let exe = exe.canonicalize().unwrap_or(exe);
    bundled_python_home_for(&exe)
}

/// The interpreter program inside a runtime home.
pub fn python_executable(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("python.exe")
    } else {
        home.join("bin/python3")
    }
}

/// The landmark CPython itself looks for: `os.py` in the standard library
/// (`Lib\` on Windows, `lib/python3.N/` elsewhere).
fn has_stdlib(home: &Path) -> bool {
    if cfg!(windows) {
        return home.join("Lib/os.py").is_file();
    }
    let Ok(entries) = std::fs::read_dir(home.join("lib")) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("python3.") && e.path().join("os.py").is_file()
    })
}

/// Drops the `..` the relative home adds, without resolving symlinks.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atd-runtime-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exe_dir(root: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            root.join("Atidarla.app/Contents/MacOS")
        } else if cfg!(windows) {
            root.to_path_buf()
        } else {
            root.join("usr/bin")
        }
    }

    fn stdlib_dir(home: &Path) -> PathBuf {
        if cfg!(windows) {
            home.join("Lib")
        } else {
            home.join("lib/python3.13")
        }
    }

    #[test]
    fn finds_the_runtime_an_installer_lays_out() {
        let root = scratch("layout");
        let bin = exe_dir(&root);
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("atidarla-app");
        let expected = normalise(&bin.join(HOME_FROM_EXE_DIR));
        assert_eq!(bundled_python_home_for(&exe), None, "no runtime yet");

        let lib = stdlib_dir(&expected);
        std::fs::create_dir_all(&lib).unwrap();
        assert_eq!(
            bundled_python_home_for(&exe),
            None,
            "a folder without os.py is not a runtime"
        );

        std::fs::write(lib.join("os.py"), "").unwrap();
        let home = bundled_python_home_for(&exe).expect("runtime found");
        assert_eq!(home, expected);
        assert!(
            !home
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_development_build_has_no_bundled_runtime() {
        // Tests run from target/<profile>/deps, which has no runtime beside it.
        assert_eq!(bundled_python_home(), None);
    }

    #[test]
    fn interpreter_path_inside_the_home() {
        let p = python_executable(Path::new("home"));
        if cfg!(windows) {
            assert_eq!(p, Path::new("home/python.exe"));
        } else {
            assert_eq!(p, Path::new("home/bin/python3"));
        }
    }
}
