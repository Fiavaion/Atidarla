//! `lasair merge-file` as a real Git merge driver, and `diff`/`merge`
//! on folders and `git:<rev>` (FR-F2, FR-F3, docs/merge.md).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_lasair");

const NETWORK: &str = r#"format = 1
next_id = 3

[[node]]
id = 0
name = "noise1"
type = "img.noise"
pos = [0.0, 0.0]
params.scale = 2.0
params.seed = 1

[[node]]
id = 1
name = "level1"
type = "img.level"
pos = [200.0, 0.0]
inputs.in = { node = 0, output = "out" }
params.gamma = 1.2
"#;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lsr-cli-merge-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git runs")
}

fn ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn has_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn project(net: &str) -> lsr_format::Project {
    lsr_format::Project::with_main("show", lsr_format::network_from_str(net).unwrap())
}

/// `NETWORK` with `old` replaced by `new`.
fn with(old: &str, new: &str) -> String {
    assert!(NETWORK.contains(old));
    NETWORK.replace(old, new)
}

fn param(dir: &Path, node: u64, name: &str) -> serde_json::Value {
    let p = lsr_format::load(dir).unwrap();
    p.main()
        .unwrap()
        .node(lsr_core::graph::NodeId(node))
        .unwrap()
        .params[name]
        .value
        .clone()
}

/// A repository with the project on `main` and the driver configured.
fn repo(name: &str) -> (PathBuf, PathBuf) {
    let repo = temp(name);
    ok(&repo, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.email", "test@example.com"),
        ("user.name", "Test"),
        ("commit.gpgsign", "false"),
        ("merge.lasair.name", "Lasair network merge"),
        (
            "merge.lasair.driver",
            &format!("\"{BIN}\" merge-file %O %A %B %P"),
        ),
    ] {
        ok(&repo, &["config", k, v]);
    }
    std::fs::write(
        repo.join(".gitattributes"),
        "*.lasair/networks/*.toml merge=lasair\n",
    )
    .unwrap();
    let dir = repo.join("show.lasair");
    lsr_format::save(&dir, &project(NETWORK)).unwrap();
    ok(&repo, &["add", "-A"]);
    ok(&repo, &["commit", "-q", "-m", "base"]);
    (repo, dir)
}

/// Commits `network` as `networks/main.toml` on a new branch from main.
fn branch(repo: &Path, dir: &Path, name: &str, network: &str) {
    ok(repo, &["checkout", "-q", "-b", name, "main"]);
    std::fs::write(dir.join("networks/main.toml"), network).unwrap();
    ok(repo, &["commit", "-q", "-am", name]);
}

#[test]
fn git_driver_merges_different_params_and_flags_the_same_one() {
    if !has_git() {
        eprintln!("git is not on PATH: skipped");
        return;
    }
    let (repo, dir) = repo("driver");
    // Different parameters of the same node: adjacent lines, which a text
    // merge would call a conflict.
    branch(
        &repo,
        &dir,
        "a",
        &with("params.scale = 2.0", "params.scale = 5.0"),
    );
    branch(
        &repo,
        &dir,
        "b",
        &with("params.seed = 1", "params.seed = 7"),
    );
    ok(&repo, &["checkout", "-q", "a"]);
    ok(&repo, &["merge", "-q", "--no-edit", "b"]);
    assert_eq!(param(&dir, 0, "scale"), serde_json::json!(5.0));
    assert_eq!(param(&dir, 0, "seed"), serde_json::json!(7));
    let text = std::fs::read_to_string(dir.join("networks/main.toml")).unwrap();
    assert!(!text.contains("<<<<<<<") && !text.contains("lasair-conflict"));

    // `diff` against a revision.
    let out = Command::new(BIN)
        .args(["diff", "git:main"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("param scale: 2.0 → 5.0"), "{report}");
    assert!(report.contains("param seed: 1 → 7"), "{report}");

    // The same parameter, two values: a conflict the app can open.
    branch(
        &repo,
        &dir,
        "c",
        &with("params.scale = 2.0", "params.scale = 3.0"),
    );
    branch(
        &repo,
        &dir,
        "d",
        &with("params.scale = 2.0", "params.scale = 9.0"),
    );
    ok(&repo, &["checkout", "-q", "c"]);
    let merged = git(&repo, &["merge", "--no-edit", "d"]);
    assert!(!merged.status.success(), "the merge stops on the conflict");
    let text = std::fs::read_to_string(dir.join("networks/main.toml")).unwrap();
    assert!(text.contains("# lasair-conflict"), "{text}");
    assert!(
        !text.contains("<<<<<<<"),
        "no text conflict markers: {text}"
    );
    assert!(
        lsr_format::load(&dir).is_err(),
        "a conflicted project does not load"
    );
    let mut m = lsr_format::merge::open_conflicted(&dir).unwrap().unwrap();
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(m.conflicts[0].kind, lsr_format::merge::ConflictKind::Param);
    m.resolve(0, Some(lsr_format::merge::Resolution::Theirs));
    lsr_format::merge::save(&dir, &m).unwrap();
    assert_eq!(param(&dir, 0, "scale"), serde_json::json!(9.0));
    ok(&repo, &["commit", "-q", "-am", "resolved"]);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn merge_command_on_folders() {
    let root = temp("folders");
    let side = |name: &str, net: &str| {
        let dir = root.join(format!("{name}.lasair"));
        lsr_format::save(&dir, &project(net)).unwrap();
        dir.to_string_lossy().into_owned()
    };
    let base = side("base", NETWORK);
    let ours = side("ours", &with("params.scale = 2.0", "params.scale = 5.0"));
    let theirs = side("theirs", &with("params.gamma = 1.2", "params.gamma = 2.2"));
    let clean = root.join("clean.lasair");
    let out = Command::new(BIN)
        .args(["merge", &base, &ours, &theirs, "--out"])
        .arg(&clean)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(param(&clean, 0, "scale"), serde_json::json!(5.0));
    assert_eq!(param(&clean, 1, "gamma"), serde_json::json!(2.2));

    let theirs = side("theirs2", &with("params.scale = 2.0", "params.scale = 9.0"));
    let conflicted = root.join("conflicted.lasair");
    let out = Command::new(BIN)
        .args(["merge", &base, &ours, &theirs, "--out"])
        .arg(&conflicted)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("CONFLICT (param) main / n0 noise1: param scale"),
        "{stdout}"
    );
    assert!(
        lsr_format::merge::open_conflicted(&conflicted)
            .unwrap()
            .is_some()
    );

    let out = Command::new(BIN)
        .args(["merge", &base, &ours, "/nonexistent.lasair"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&root);
}
