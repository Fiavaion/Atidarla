//! macOS: builds the official Syphon framework from source for
//! `lsr_interop::syphon`. Other targets: nothing.
//!
//! The source is Syphon-Framework (BSD-3-Clause, see
//! `third-party/Syphon/License.txt`) at [`SYPHON_COMMIT`]. It is not
//! vendored: the build fetches that commit (`git`, shallow) into `OUT_DIR`,
//! or uses the checkout named by `LSR_SYPHON_SRC` (offline builds). It is
//! built unmodified with `xcodebuild` into `OUT_DIR`, and nothing is linked:
//! `lsr_interop::syphon` loads the framework at run time from the path in
//! `LSR_SYPHON_FRAMEWORK`, so binaries start even where it is missing.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const SYPHON_REPO: &str = "https://github.com/Syphon/Syphon-Framework";
/// `main` on 2026-10-30 ("Small fix so setting a SyphonServer.name
/// correctly copies the new name ...").
const SYPHON_COMMIT: &str = "f4761677a45b8034a3c2069ec0f3d2553da81fba";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LSR_SYPHON_SRC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let src = match env::var_os("LSR_SYPHON_SRC") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let dir = out.join("syphon-src");
            fetch(&dir);
            dir
        }
    };
    let framework = build(&src, &out);
    println!(
        "cargo:rustc-env=LSR_SYPHON_FRAMEWORK={}",
        framework.display()
    );
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{what}: cannot run {cmd:?}: {e}"));
    assert!(status.success(), "{what} failed ({status}): {cmd:?}");
}

/// The commit checked out in `dir`, if it is a git checkout.
fn head(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn fetch(dir: &Path) {
    if head(dir).as_deref() == Some(SYPHON_COMMIT) {
        return;
    }
    let what = format!(
        "fetching Syphon-Framework (set LSR_SYPHON_SRC to a checkout of commit \
         {SYPHON_COMMIT} to build offline)"
    );
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create the Syphon source directory");
    let git = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(dir).args(args);
        run(&mut cmd, &what);
    };
    git(&["init", "-q"]);
    git(&["fetch", "-q", "--depth", "1", SYPHON_REPO, SYPHON_COMMIT]);
    git(&["checkout", "-q", "--detach", "FETCH_HEAD"]);
}

fn build(src: &Path, out: &Path) -> PathBuf {
    let project = src.join("Syphon.xcodeproj");
    assert!(
        project.exists(),
        "no Syphon.xcodeproj in {} (LSR_SYPHON_SRC must be a Syphon-Framework checkout)",
        src.display()
    );
    println!("cargo:rerun-if-changed={}", src.display());
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("Syphon: unsupported macOS architecture {other:?}"),
    };
    let sym = out.join("syphon-build");
    let mut cmd = Command::new("xcodebuild");
    cmd.arg("-project")
        .arg(&project)
        .args(["-target", "Syphon", "-configuration", "Release", "-quiet"])
        .arg(format!("ARCHS={arch}"))
        .arg("ONLY_ACTIVE_ARCH=NO")
        .arg(format!("SYMROOT={}", sym.display()))
        .arg(format!("OBJROOT={}", out.join("syphon-obj").display()))
        .arg("build");
    run(
        &mut cmd,
        "building Syphon.framework with xcodebuild (needs Xcode with its Metal \
         toolchain; if the Metal compiler is missing: \
         xcodebuild -downloadComponent MetalToolchain)",
    );
    let framework = sym.join("Release").join("Syphon.framework");
    assert!(
        framework.join("Syphon").exists(),
        "xcodebuild did not produce {}",
        framework.display()
    );
    framework
}
