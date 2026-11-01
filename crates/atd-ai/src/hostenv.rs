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
//!
//! `diffusion` and `diffusion-cuda` hold the same packages; the second takes
//! torch from PyTorch's CUDA index on Windows and Linux (PRD A70), where
//! PyPI's Windows torch is CPU only. One replaces the other when installed.
//! [`recommended_extras`] picks the extras a machine should get, CUDA only
//! when its NVIDIA driver is new enough ([`nvidia_driver`]).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::client::HostCommand;

/// The diffusion extra with PyPI's torch (CPU on Windows, Metal on macOS).
pub const DIFFUSION: &str = "diffusion";
/// The diffusion extra with PyTorch's CUDA build on Windows and Linux.
pub const DIFFUSION_CUDA: &str = "diffusion-cuda";
/// The index `diffusion-cuda` takes torch from (`ai-host/pyproject.toml`).
pub const PYTORCH_INDEX: &str = "https://download.pytorch.org/whl/cu134";
/// The CUDA version of that build.
pub const CUDA_BUILD: &str = "CUDA 13.4";
/// The oldest NVIDIA driver that runs it: every CUDA 13.x build runs on
/// R580 or later (NVIDIA's minor-version compatibility). Covers Turing to
/// Blackwell (RTX 20 to RTX 50).
pub const CUDA_DRIVER_FLOOR: (u32, u32) = (580, 0);
/// How long [`nvidia_driver`] waits for `nvidia-smi`.
const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_secs(2);

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
         reach PyPI and download.pytorch.org (`--allow-internet`), or install offline from a folder of wheels \
         (`--wheelhouse <folder>`; see docs/install.md)"
    )]
    Sovereign,
    #[error("{0} is not a folder")]
    NoWheelhouse(PathBuf),
    #[error("install one of `{0}` and `{1}`, not both")]
    Exclusive(String, String),
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
    match source {
        Source::Wheelhouse(dir) => {
            cmd.arg("--no-index").arg("--find-links").arg(dir);
        }
        // The hashes still pin every file, whichever index serves it.
        Source::Index if extras.iter().any(|e| e == DIFFUSION_CUDA) => {
            cmd.args(["--extra-index-url", PYTORCH_INDEX]);
        }
        Source::Index => {}
    }
    for extra in extras {
        cmd.arg("-r").arg(bundled.requirements(extra));
    }
    cmd
}

/// The extra that cannot be installed beside `extra`, if any.
fn rival(extra: &str) -> Option<&'static str> {
    match extra {
        DIFFUSION => Some(DIFFUSION_CUDA),
        DIFFUSION_CUDA => Some(DIFFUSION),
        _ => None,
    }
}

/// The extras recorded after installing `requested` over `installed`,
/// sorted: a requested extra replaces its rival (`diffusion` and
/// `diffusion-cuda`), and asking for both is refused.
pub fn merge_extras(
    installed: &[String],
    requested: &[String],
) -> Result<Vec<String>, InstallError> {
    for extra in requested {
        if let Some(rival) = rival(extra)
            && requested.iter().any(|e| e == rival)
        {
            return Err(InstallError::Exclusive(
                DIFFUSION.into(),
                DIFFUSION_CUDA.into(),
            ));
        }
    }
    let mut all: Vec<String> = installed
        .iter()
        .filter(|e| !requested.iter().any(|r| rival(r) == Some(e.as_str())))
        .chain(requested)
        .cloned()
        .collect();
    all.sort();
    all.dedup();
    Ok(all)
}

/// Installs `extras` (with every extra already installed, so they share one
/// consistent set of locked packages) into a fresh folder, then swaps it in.
/// `diffusion` and `diffusion-cuda` replace each other ([`merge_extras`]).
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
    let packages = bundled.packages(root);
    let mut installed = packages.installed();
    installed.retain(|e| known.contains(e));
    let all = merge_extras(&installed, extras)?;
    match source {
        Source::Index if crate::egress::sovereign() && !allow_internet_once => {
            return Err(InstallError::Sovereign);
        }
        Source::Wheelhouse(dir) if !dir.is_dir() => {
            return Err(InstallError::NoWheelhouse(dir.clone()));
        }
        _ => {}
    }

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

/// The NVIDIA driver's `(major, minor)` version, from `nvidia-smi`, or `None`
/// without one (always on macOS). Gives up after two seconds. It starts a
/// process: call it from a command or a worker thread, never the render
/// thread.
pub fn nvidia_driver() -> Option<(u32, u32)> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let mut cmd = Command::new("nvidia-smi");
    cmd.args(["--query-gpu=driver_version", "--format=csv,noheader"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + NVIDIA_SMI_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(Some(_)) => return None,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_driver_version(&out)
}

/// The first `major.minor[.patch]` line of `nvidia-smi`'s output (one line
/// per GPU, all the same driver): `566.36` on Windows, `580.65.06` on Linux.
fn parse_driver_version(out: &str) -> Option<(u32, u32)> {
    out.lines().find_map(|line| {
        let mut parts = line.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        Some((major, minor))
    })
}

fn driver_text((major, minor): (u32, u32)) -> String {
    if minor == 0 {
        major.to_string()
    } else {
        format!("{major}.{minor:02}")
    }
}

/// The extras a machine should get, and one sentence saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    /// Sorted; `cloud` is never recommended (it needs the user's own key).
    pub extras: Vec<String>,
    pub reason: String,
}

/// The extras this machine should get from `bundled`: `vision`, `stt` and
/// one diffusion extra, `diffusion-cuda` on Windows or Linux with an NVIDIA
/// driver of [`CUDA_DRIVER_FLOOR`] or later, else `diffusion`. Runs
/// `nvidia-smi` off macOS ([`nvidia_driver`]), so never call it on the
/// render thread.
pub fn recommended_extras(bundled: &Bundled) -> Recommendation {
    let cuda_platform = cfg!(any(windows, target_os = "linux"));
    let driver = if cuda_platform { nvidia_driver() } else { None };
    recommend(&bundled.extras(), cuda_platform, driver)
}

/// [`recommended_extras`] for given facts: the extras a build offers,
/// whether this platform has CUDA builds at all (Windows, Linux) and the
/// NVIDIA driver found.
pub fn recommend(
    available: &[String],
    cuda_platform: bool,
    driver: Option<(u32, u32)>,
) -> Recommendation {
    let has = |e: &str| available.iter().any(|a| a == e);
    let floor = driver_text(CUDA_DRIVER_FLOOR);
    let (diffusion, reason) = match driver {
        _ if !cuda_platform => (
            DIFFUSION,
            "PyTorch's standard build runs diffusion on this platform (Metal on macOS); CUDA \
             builds are for Windows and Linux."
                .to_owned(),
        ),
        None => (
            DIFFUSION,
            format!(
                "No NVIDIA driver found; installing PyTorch's standard build (CPU). Real-time \
                 diffusion needs an NVIDIA GPU with driver {floor} or later."
            ),
        ),
        Some(d) if d < CUDA_DRIVER_FLOOR => (
            DIFFUSION,
            format!(
                "NVIDIA driver {} is older than {floor} needed for {CUDA_BUILD}; installing the \
                 CPU build. Update the driver for real-time diffusion.",
                driver_text(d)
            ),
        ),
        Some(d) if !has(DIFFUSION_CUDA) => (
            DIFFUSION,
            format!(
                "NVIDIA driver {} found, but this build has no `{DIFFUSION_CUDA}` extra; \
                 installing the CPU build.",
                driver_text(d)
            ),
        ),
        Some(d) => (
            DIFFUSION_CUDA,
            format!(
                "NVIDIA driver {} runs {CUDA_BUILD}; installing PyTorch's CUDA build for \
                 real-time diffusion.",
                driver_text(d)
            ),
        ),
    };
    let mut extras: Vec<String> = ["vision", "stt", diffusion]
        .into_iter()
        .filter(|e| has(e))
        .map(str::to_owned)
        .collect();
    extras.sort();
    Recommendation { extras, reason }
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

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn only_the_cuda_extra_reaches_pytorchs_index() {
        let root = scratch("pip-cuda");
        let b = layout(&root);
        let args = |extras: &[&str], source: &Source| -> Vec<String> {
            pip_command(&b, &strings(extras), &root.join("t"), source)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let cuda = args(&["diffusion-cuda", "vision"], &Source::Index);
        let at = cuda.iter().position(|a| a == "--extra-index-url").unwrap();
        assert_eq!(cuda[at + 1], PYTORCH_INDEX);
        assert!(cuda.iter().any(|a| a == "--require-hashes"));
        for plain in [args(&["diffusion", "vision"], &Source::Index), {
            args(&["diffusion-cuda"], &Source::Wheelhouse(root.join("w")))
        }] {
            assert!(
                !plain
                    .iter()
                    .any(|a| a.contains("index-url") || a.contains("pytorch")),
                "{plain:?}"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diffusion_and_diffusion_cuda_replace_each_other() {
        let merge = |installed: &[&str], requested: &[&str]| {
            merge_extras(&strings(installed), &strings(requested))
        };
        assert_eq!(
            merge(&["diffusion", "vision"], &["diffusion-cuda"]).unwrap(),
            ["diffusion-cuda", "vision"]
        );
        assert_eq!(
            merge(&["diffusion-cuda", "stt"], &["diffusion"]).unwrap(),
            ["diffusion", "stt"]
        );
        assert_eq!(
            merge(&["diffusion-cuda"], &["diffusion-cuda", "stt"]).unwrap(),
            ["diffusion-cuda", "stt"]
        );
        assert_eq!(merge(&["vision"], &["stt"]).unwrap(), ["stt", "vision"]);
        let err = merge(&[], &["diffusion", "diffusion-cuda"]).unwrap_err();
        assert!(matches!(err, InstallError::Exclusive(..)), "{err}");
    }

    #[test]
    fn install_refuses_both_diffusion_extras_before_touching_anything() {
        let root = scratch("exclusive");
        let b = layout(&root);
        for e in [DIFFUSION, DIFFUSION_CUDA] {
            std::fs::write(b.requirements(e), "").unwrap();
        }
        let data = root.join("data");
        let both = strings(&[DIFFUSION, DIFFUSION_CUDA]);
        let err = install(&b, &data, &both, &Source::Wheelhouse(root.clone()), false).unwrap_err();
        assert!(matches!(err, InstallError::Exclusive(..)), "{err}");
        assert!(!data.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nvidia_smi_output_parses_on_windows_and_linux() {
        assert_eq!(parse_driver_version("566.36\n"), Some((566, 36)));
        assert_eq!(
            parse_driver_version("580.65.06\r\n580.65.06\n"),
            Some((580, 65))
        );
        assert_eq!(parse_driver_version("\n  591.44 \n"), Some((591, 44)));
        assert_eq!(parse_driver_version("NVIDIA-SMI has failed\n"), None);
        assert_eq!(parse_driver_version(""), None);
        if cfg!(target_os = "macos") {
            assert_eq!(nvidia_driver(), None);
        }
    }

    #[test]
    fn the_recommendation_follows_the_platform_and_the_driver() {
        let all = strings(&["cloud", "diffusion", "diffusion-cuda", "stt", "vision"]);
        let rec = recommend(&all, true, Some((581, 57)));
        assert_eq!(rec.extras, ["diffusion-cuda", "stt", "vision"]);
        assert!(
            rec.reason.contains("581.57") && rec.reason.contains(CUDA_BUILD),
            "{}",
            rec.reason
        );
        // The floor itself is enough.
        assert!(
            recommend(&all, true, Some(CUDA_DRIVER_FLOOR))
                .extras
                .contains(&DIFFUSION_CUDA.into())
        );

        let old = recommend(&all, true, Some((566, 36)));
        assert_eq!(old.extras, ["diffusion", "stt", "vision"]);
        assert_eq!(
            old.reason,
            "NVIDIA driver 566.36 is older than 580 needed for CUDA 13.4; installing the CPU \
             build. Update the driver for real-time diffusion."
        );
        let none = recommend(&all, true, None);
        assert_eq!(none.extras, ["diffusion", "stt", "vision"]);
        assert!(
            none.reason.starts_with("No NVIDIA driver found"),
            "{}",
            none.reason
        );

        // macOS: always the standard build, whatever a driver query says.
        let mac = recommend(&all, false, Some((600, 0)));
        assert_eq!(mac.extras, ["diffusion", "stt", "vision"]);
        assert!(mac.reason.contains("Metal"), "{}", mac.reason);

        // Only what the build offers; an older build without the CUDA extra.
        let older = strings(&["diffusion", "vision"]);
        let rec = recommend(&older, true, Some((581, 57)));
        assert_eq!(rec.extras, ["diffusion", "vision"]);
        assert!(rec.reason.contains("no `diffusion-cuda`"), "{}", rec.reason);
        assert!(recommend(&[], true, None).extras.is_empty());
    }

    #[test]
    fn the_cuda_constants_match_the_ai_host_project() {
        let pyproject = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ai-host/pyproject.toml"
        ))
        .unwrap();
        assert!(
            pyproject.contains(&format!("url = \"{PYTORCH_INDEX}\"")),
            "PYTORCH_INDEX is not the index in ai-host/pyproject.toml"
        );
        let digits = PYTORCH_INDEX.rsplit_once("/cu").unwrap().1;
        let (major, minor) = digits.split_at(digits.len() - 1);
        assert_eq!(CUDA_BUILD, format!("CUDA {major}.{minor}"));
        if major.starts_with("13") {
            assert_eq!(
                CUDA_DRIVER_FLOOR,
                (580, 0),
                "CUDA 13.x runs on R580 or later"
            );
        }
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
