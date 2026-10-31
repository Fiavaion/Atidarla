//! The AI Host an installer ships (PRD A68) and its optional packages.
//!
//! The DMG, MSI and AppImage carry the host's source (`ai-host/src`), its
//! lock file and one hash-pinned requirements file per optional extra
//! (`ai-host/requirements/<extra>.txt`, exported from `uv.lock`), next to
//! the bundled CPython ([`atd_core::runtime`]). The host's base install is
//! standard library only, so the built-in adapter runs from the bundle as
//! it is.
//!
//! Optional extras (`vision`, `stt`, `diffusion`, `cloud`) are installed
//! only when the user asks (`atidarla ai-host install <extra>`), with the
//! bundled Python's own pip, into a per-user folder, never the installed
//! app (a signed macOS bundle must not change):
//!
//! - macOS: `~/Library/Application Support/Atidarla/ai-host`;
//! - Windows: `%LOCALAPPDATA%\Atidarla\ai-host`;
//! - Linux: `$XDG_DATA_HOME/atidarla/ai-host` (else `~/.local/share/...`).
//!
//! pip runs with `--require-hashes --no-deps --only-binary :all:`: exactly
//! the locked wheels, nothing built, nothing unpinned. It reaches PyPI only
//! with an explicit `allow_internet_once` (Sovereign mode refuses
//! otherwise); the offline route is a wheelhouse folder (`--no-index
//! --find-links`). Nothing here runs on start or on the render thread.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::client::HostCommand;

/// The folder an installer's AI Host keeps its packages in, per user.
pub fn data_root() -> PathBuf {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = var("HOME").or_else(|| var("USERPROFILE"));
    let base = if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/Atidarla"))
    } else if cfg!(windows) {
        var("LOCALAPPDATA")
            .or_else(|| home.map(|h| h.join("AppData").join("Local")))
            .map(|d| d.join("Atidarla"))
    } else {
        var("XDG_DATA_HOME")
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".local/share")))
            .map(|d| d.join("atidarla"))
    };
    base.unwrap_or_else(|| std::env::temp_dir().join("atidarla"))
        .join("ai-host")
}

/// The AI Host an installer put beside the executable, with its runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundled {
    /// The `ai-host` folder (pyproject.toml, uv.lock, src/, requirements/).
    pub dir: PathBuf,
    /// The bundled CPython's home (`sys.prefix`).
    pub python_home: PathBuf,
    /// Its `major.minor` version: user packages are kept per version.
    pub python_version: String,
}

impl Bundled {
    /// The bundled host of this process, if it is an installed app.
    pub fn find() -> Option<Self> {
        Self::at(
            atd_core::runtime::bundled_ai_host()?,
            atd_core::runtime::bundled_python_home()?,
        )
    }

    /// A host folder and runtime home laid out as an installer does.
    pub fn at(dir: PathBuf, python_home: PathBuf) -> Option<Self> {
        let python_version = atd_core::runtime::python_version(&python_home)?;
        Some(Self {
            dir,
            python_home,
            python_version,
        })
    }

    pub fn python(&self) -> PathBuf {
        atd_core::runtime::python_executable(&self.python_home)
    }

    /// The extras this build can install, sorted (`requirements/*.txt`).
    pub fn extras(&self) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(self.dir.join("requirements"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".txt").map(str::to_owned)
            })
            .collect();
        out.sort();
        out
    }

    pub fn requirements(&self, extra: &str) -> PathBuf {
        self.dir.join("requirements").join(format!("{extra}.txt"))
    }

    /// The per-user packages of this runtime version under `root`.
    pub fn packages(&self, root: &Path) -> Packages {
        Packages {
            dir: root.join(format!("python{}", self.python_version)),
        }
    }

    /// The local host command: the bundled Python running the bundled
    /// source, with the user's installed extras (if any) on its path. The
    /// environment is fixed so nothing from the machine leaks in
    /// (`PYTHONHOME`, user site-packages) and no bytecode is written: the
    /// bundle ships its `.pyc` precompiled.
    pub fn host_command(&self, root: &Path) -> HostCommand {
        let mut path = vec![self.dir.join("src")];
        let site = self.packages(root).site();
        if site.is_dir() {
            path.push(site);
        }
        let pythonpath =
            std::env::join_paths(&path).unwrap_or_else(|_| path[0].clone().into_os_string());
        HostCommand {
            program: self.python().into_os_string(),
            args: vec!["-m".into(), "atd_ai_host".into()],
            env: vec![
                ("PYTHONPATH".into(), pythonpath),
                (
                    "PYTHONHOME".into(),
                    self.python_home.clone().into_os_string(),
                ),
                ("PYTHONNOUSERSITE".into(), "1".into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ],
        }
    }
}

/// Installed extras for one runtime version: `<dir>/site-packages` and the
/// list of extras in it, `<dir>/extras.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packages {
    pub dir: PathBuf,
}

impl Packages {
    pub fn site(&self) -> PathBuf {
        self.dir.join("site-packages")
    }

    fn record(&self) -> PathBuf {
        self.dir.join("extras.txt")
    }

    /// The extras installed, sorted.
    pub fn installed(&self) -> Vec<String> {
        if !self.site().is_dir() {
            return vec![];
        }
        let mut out: Vec<String> = std::fs::read_to_string(self.record())
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// Where pip takes the wheels from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The Python Package Index (needs the internet).
    Index,
    /// A folder of wheels prepared on another machine (offline).
    Wheelhouse(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("this build has no AI Host extra `{extra}` (known: {known})")]
    UnknownExtra { extra: String, known: String },
    #[error(
        "Sovereign mode blocks downloading packages from the internet. Allow this one install to \
         reach PyPI (`--allow-internet`), or install offline from a folder of wheels \
         (`--wheelhouse <folder>`; see docs/install.md)"
    )]
    Sovereign,
    #[error("{0} is not a folder")]
    NoWheelhouse(PathBuf),
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    #[error("pip failed ({0}); the installed extras are unchanged")]
    Pip(std::process::ExitStatus),
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> InstallError {
    let what = what.into();
    move |source| InstallError::Io { what, source }
}

/// The pip command that installs `extras` into `target`.
pub fn pip_command(
    bundled: &Bundled,
    extras: &[String],
    target: &Path,
    source: &Source,
) -> Command {
    let mut cmd = Command::new(bundled.python());
    cmd.env("PYTHONHOME", &bundled.python_home)
        .env("PYTHONNOUSERSITE", "1")
        .env_remove("PYTHONPATH")
        .args([
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--no-input",
        ])
        .args(["--require-hashes", "--no-deps", "--only-binary", ":all:"])
        .arg("--target")
        .arg(target);
    if let Source::Wheelhouse(dir) = source {
        cmd.arg("--no-index").arg("--find-links").arg(dir);
    }
    for extra in extras {
        cmd.arg("-r").arg(bundled.requirements(extra));
    }
    cmd
}

/// Installs `extras` (with every extra already installed, so they share one
/// consistent set of locked packages) into a fresh folder, then swaps it in.
/// A failed install leaves the previous packages as they were. Blocks while
/// pip runs; call it from a command or a worker thread, never the render
/// thread. Returns the extras now installed.
pub fn install(
    bundled: &Bundled,
    root: &Path,
    extras: &[String],
    source: &Source,
    allow_internet_once: bool,
) -> Result<Vec<String>, InstallError> {
    let known = bundled.extras();
    for extra in extras {
        if !known.contains(extra) {
            return Err(InstallError::UnknownExtra {
                extra: extra.clone(),
                known: known.join(", "),
            });
        }
    }
    match source {
        Source::Index if crate::egress::sovereign() && !allow_internet_once => {
            return Err(InstallError::Sovereign);
        }
        Source::Wheelhouse(dir) if !dir.is_dir() => {
            return Err(InstallError::NoWheelhouse(dir.clone()));
        }
        _ => {}
    }
    let packages = bundled.packages(root);
    let mut all = packages.installed();
    all.retain(|e| known.contains(e));
    all.extend(extras.iter().cloned());
    all.sort();
    all.dedup();

    std::fs::create_dir_all(&packages.dir)
        .map_err(io(format!("creating {}", packages.dir.display())))?;
    let fresh = packages.dir.join("site-packages.new");
    let old = packages.dir.join("site-packages.old");
    for leftover in [&fresh, &old] {
        if leftover.exists() {
            std::fs::remove_dir_all(leftover)
                .map_err(io(format!("removing {}", leftover.display())))?;
        }
    }
    let status = pip_command(bundled, &all, &fresh, source)
        .status()
        .map_err(io(format!("starting {}", bundled.python().display())))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(InstallError::Pip(status));
    }
    let site = packages.site();
    if site.exists() {
        std::fs::rename(&site, &old).map_err(io(format!("moving {}", site.display())))?;
    }
    std::fs::rename(&fresh, &site).map_err(io(format!("moving {}", fresh.display())))?;
    let mut record = all.join("\n");
    record.push('\n');
    std::fs::write(packages.record(), record)
        .map_err(io(format!("writing {}", packages.record().display())))?;
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(all)
}

/// Removes every installed extra (the built-in adapter keeps working).
pub fn remove_all(bundled: &Bundled, root: &Path) -> Result<(), InstallError> {
    let packages = bundled.packages(root);
    if packages.dir.exists() {
        std::fs::remove_dir_all(&packages.dir)
            .map_err(io(format!("removing {}", packages.dir.display())))?;
    }
    Ok(())
}

/// `PYTHONPATH` of a host command, split (for tests and status output).
pub fn python_path(cmd: &HostCommand) -> Vec<PathBuf> {
    cmd.env
        .iter()
        .find(|(k, _)| k == &OsString::from("PYTHONPATH"))
        .map(|(_, v)| std::env::split_paths(v).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atd-hostenv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An installer-shaped host folder and runtime home (no real Python).
    fn layout(root: &Path) -> Bundled {
        let dir = root.join("ai-host");
        std::fs::create_dir_all(dir.join("src/atd_ai_host")).unwrap();
        std::fs::create_dir_all(dir.join("requirements")).unwrap();
        for extra in ["vision", "stt"] {
            std::fs::write(dir.join(format!("requirements/{extra}.txt")), "").unwrap();
        }
        let home = root.join("python");
        if cfg!(windows) {
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(home.join("python313.dll"), "").unwrap();
        } else {
            std::fs::create_dir_all(home.join("lib/python3.13")).unwrap();
            std::fs::write(home.join("lib/python3.13/os.py"), "").unwrap();
        }
        Bundled::at(dir, home).expect("layout")
    }

    #[test]
    fn the_host_command_runs_the_bundled_source_on_the_bundled_python() {
        let root = scratch("cmd");
        let b = layout(&root);
        let data = root.join("data");
        let cmd = b.host_command(&data);
        assert_eq!(PathBuf::from(&cmd.program), b.python());
        assert_eq!(cmd.args, ["-m", "atd_ai_host"]);
        assert_eq!(python_path(&cmd), [b.dir.join("src")], "no extras yet");
        let env = |k: &str| {
            cmd.env
                .iter()
                .find(|(n, _)| n == &OsString::from(k))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(env("PYTHONHOME"), Some(b.python_home.clone().into()));
        assert_eq!(env("PYTHONDONTWRITEBYTECODE"), Some("1".into()));
        assert_eq!(env("PYTHONNOUSERSITE"), Some("1".into()));

        // Installed extras join the path after the source.
        let site = b.packages(&data).site();
        assert_eq!(site, data.join("python3.13/site-packages"));
        std::fs::create_dir_all(&site).unwrap();
        assert_eq!(
            python_path(&b.host_command(&data)),
            [b.dir.join("src"), site]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn extras_come_from_the_shipped_requirements() {
        let root = scratch("extras");
        let b = layout(&root);
        assert_eq!(b.extras(), ["stt", "vision"]);
        assert_eq!(b.requirements("stt"), b.dir.join("requirements/stt.txt"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Touches the process-wide Sovereign flag.
    #[test]
    fn sovereign_mode_refuses_the_index_and_unknown_extras_are_named() {
        let _flags = crate::egress::TEST_FLAGS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::egress::set_sovereign(true);
        let root = scratch("sovereign");
        let b = layout(&root);
        let data = root.join("data");
        let err = install(&b, &data, &["vision".into()], &Source::Index, false).unwrap_err();
        assert!(matches!(err, InstallError::Sovereign), "{err}");
        assert!(
            err.to_string().contains("--wheelhouse"),
            "names the offline route"
        );
        let err = install(&b, &data, &["torch".into()], &Source::Index, true).unwrap_err();
        assert!(err.to_string().contains("known: stt, vision"), "{err}");
        let err = install(
            &b,
            &data,
            &["vision".into()],
            &Source::Wheelhouse(root.join("missing")),
            false,
        )
        .unwrap_err();
        assert!(matches!(err, InstallError::NoWheelhouse(_)), "{err}");
        assert!(!data.exists(), "nothing written when refused");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pip_is_pinned_to_the_lock_and_offline_with_a_wheelhouse() {
        let root = scratch("pip");
        let b = layout(&root);
        let wheels = root.join("wheels");
        let cmd = pip_command(
            &b,
            &["stt".into(), "vision".into()],
            &root.join("t"),
            &Source::Wheelhouse(wheels.clone()),
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for flag in [
            "--require-hashes",
            "--no-deps",
            "--no-index",
            "--disable-pip-version-check",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag} in {args:?}");
        }
        let at = |s: &str| args.iter().position(|a| a == s).unwrap();
        assert_eq!(args[at("--only-binary") + 1], ":all:");
        assert_eq!(PathBuf::from(&args[at("--find-links") + 1]), wheels);
        let reqs: Vec<&String> = args
            .iter()
            .zip(args.iter().skip(1))
            .filter(|(a, _)| *a == "-r")
            .map(|(_, r)| r)
            .collect();
        assert_eq!(reqs.len(), 2);
        let index = pip_command(&b, &["stt".into()], &root.join("t"), &Source::Index);
        assert!(!index.get_args().any(|a| a == "--no-index"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn installed_extras_are_read_only_with_their_packages() {
        let root = scratch("installed");
        let b = layout(&root);
        let p = b.packages(&root.join("data"));
        assert!(p.installed().is_empty());
        std::fs::create_dir_all(&p.dir).unwrap();
        std::fs::write(p.dir.join("extras.txt"), "vision\nstt\n").unwrap();
        assert!(p.installed().is_empty(), "a record without packages");
        std::fs::create_dir_all(p.site()).unwrap();
        assert_eq!(p.installed(), ["stt", "vision"]);
        remove_all(&b, &root.join("data")).unwrap();
        assert!(p.installed().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_data_root_is_per_user_not_the_app() {
        let root = data_root();
        assert!(root.ends_with("ai-host"), "{}", root.display());
        if let Ok(exe) = std::env::current_exe() {
            assert!(!root.starts_with(exe.parent().unwrap()));
        }
    }
}
