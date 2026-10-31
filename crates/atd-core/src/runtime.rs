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
//!
//! The installers also ship the AI Host's source beside the runtime
//! ([`bundled_ai_host`]): `Contents/Resources/ai-host`, `<install
//! folder>\ai-host` or `usr/lib/atidarla/ai-host`.

use std::path::{Path, PathBuf};

/// The runtime home relative to the folder holding the executable.
const HOME_FROM_EXE_DIR: &str = if cfg!(target_os = "macos") {
    "../Resources/python"
} else if cfg!(windows) {
    "python"
} else {
    "../lib/atidarla/python"
};

/// The bundled AI Host folder (pyproject, lock, `src/`, `requirements/`)
/// relative to the folder holding the executable.
const AI_HOST_FROM_EXE_DIR: &str = if cfg!(target_os = "macos") {
    "../Resources/ai-host"
} else if cfg!(windows) {
    "ai-host"
} else {
    "../lib/atidarla/ai-host"
};

/// The bundled runtime's home (its `sys.prefix`) for an executable at
/// `exe`, if the installer put one there.
pub fn bundled_python_home_for(exe: &Path) -> Option<PathBuf> {
    let home = exe.parent()?.join(HOME_FROM_EXE_DIR);
    has_stdlib(&home).then(|| normalise(&home))
}

/// The bundled runtime's home for this process, if there is one.
pub fn bundled_python_home() -> Option<PathBuf> {
    bundled_python_home_for(&current_exe()?)
}

/// The bundled AI Host folder for an executable at `exe`, if the installer
/// put one there (its landmark is `src/atd_ai_host/__main__.py`).
pub fn bundled_ai_host_for(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?.join(AI_HOST_FROM_EXE_DIR);
    dir.join("src/atd_ai_host/__main__.py")
        .is_file()
        .then(|| normalise(&dir))
}

/// The bundled AI Host folder for this process, if there is one.
pub fn bundled_ai_host() -> Option<PathBuf> {
    bundled_ai_host_for(&current_exe()?)
}

/// This executable, with a symlink on the PATH (`ln -s
/// .../Contents/MacOS/atidarla`) resolved to its target in the bundle.
fn current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.canonicalize().unwrap_or(exe))
}

/// The runtime's `major.minor` version (`3.13`), read from its layout:
/// `lib/python3.N/` on macOS and Linux, `python3NN.dll` on Windows.
pub fn python_version(home: &Path) -> Option<String> {
    let entries = std::fs::read_dir(if cfg!(windows) {
        home.to_path_buf()
    } else {
        home.join("lib")
    })
    .ok()?;
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if cfg!(windows) {
                let digits = name.strip_prefix("python3")?.strip_suffix(".dll")?;
                (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| format!("3.{digits}"))
            } else {
                let minor = name.strip_prefix("python3.")?;
                (!minor.is_empty()
                    && minor.bytes().all(|b| b.is_ascii_digit())
                    && e.path().join("os.py").is_file())
                .then(|| format!("3.{minor}"))
            }
        })
        .collect();
    found.sort();
    found.pop()
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
        assert_eq!(bundled_ai_host(), None);
    }

    #[test]
    fn finds_the_ai_host_an_installer_lays_out() {
        let root = scratch("ai-host");
        let bin = exe_dir(&root);
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("atidarla-app");
        let expected = normalise(&bin.join(AI_HOST_FROM_EXE_DIR));
        assert_eq!(bundled_ai_host_for(&exe), None, "no host yet");
        let pkg = expected.join("src/atd_ai_host");
        std::fs::create_dir_all(&pkg).unwrap();
        assert_eq!(
            bundled_ai_host_for(&exe),
            None,
            "a package without __main__.py"
        );
        std::fs::write(pkg.join("__main__.py"), "").unwrap();
        assert_eq!(bundled_ai_host_for(&exe), Some(expected));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_the_runtime_version_from_its_layout() {
        let home = scratch("version");
        assert_eq!(python_version(&home), None);
        if cfg!(windows) {
            std::fs::write(home.join("python3.dll"), "").unwrap();
            std::fs::write(home.join("python313.dll"), "").unwrap();
        } else {
            let lib = stdlib_dir(&home);
            std::fs::create_dir_all(&lib).unwrap();
            std::fs::write(lib.join("os.py"), "").unwrap();
            // Not a standard library: no os.py.
            std::fs::create_dir_all(home.join("lib/python3.14")).unwrap();
        }
        assert_eq!(python_version(&home).as_deref(), Some("3.13"));
        std::fs::remove_dir_all(&home).unwrap();
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
