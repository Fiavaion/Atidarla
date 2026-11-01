//! Headless commands run a project's Python expressions only with
//! `--allow-python-ops` (docs/python-ops.md): opening a project someone sent
//! must not run its code. Each run is its own process, as the allowance is
//! process-wide. The project's expression writes a marker file, so "did not
//! run" is checked directly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lsr_core::graph::{Edit, Endpoint, Network, Node, Param, Wire};
use lsr_core::registry::Registry;
use serde_json::json;

const BIN: &str = env!("CARGO_BIN_EXE_lasair");

fn gpu() -> bool {
    if lsr_gpu::GpuContext::headless().is_ok() {
        return true;
    }
    assert!(
        !std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
        "LSR_REQUIRE_GPU is set but there is no GPU adapter"
    );
    false
}

/// A project whose only output follows a Python expression that writes
/// `marker` and gives 0.5.
fn project(root: &Path, marker: &Path) -> PathBuf {
    let dir = root.join("gate.lasair");
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    let mut net = Network::default();
    let (white, level) = (net.alloc_id(), net.alloc_id());
    let source = format!(
        "__import__('pathlib').Path({:?}).write_text('ran') * 0 + 0.5",
        marker.display().to_string()
    );
    for edit in [
        Edit::AddNode {
            node: Node::new(white, "img.constant", "white1"),
        },
        Edit::AddNode {
            node: Node::new(level, "img.level", "level1"),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(white, "out"), Endpoint::new(level, "in")),
        },
        Edit::SetParam {
            node: level,
            param: "brightness".into(),
            value: Some(Param {
                value: json!(1.0),
                expr: Some(source),
                bind: None,
            }),
        },
    ] {
        net.apply(&reg, edit).unwrap();
    }
    let mut p = lsr_format::Project::new("Gate");
    p.resolution = [16, 16];
    p.networks.insert(lsr_format::MAIN.to_owned(), net);
    lsr_format::save(&dir, &p).unwrap();
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("running lasair")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn headless_commands_run_python_expressions_only_when_allowed() {
    if !gpu() {
        return;
    }
    let root = std::env::temp_dir().join(format!("lsr-python-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let marker = root.join("marker");
    let dir = project(&root, &marker);
    let dir_s = dir.to_str().unwrap();
    let out = root.join("frames");
    let out_s = out.to_str().unwrap();
    let ran = |m: &Path| {
        let yes = m.exists();
        let _ = std::fs::remove_file(m);
        yes
    };

    // check: passes, lists the expression as not evaluated, runs nothing.
    let o = run(&["check", "--gpu", "--frames", "20", dir_s]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("Python expressions not evaluated") && text(&o).contains("level1"),
        "{}",
        text(&o)
    );
    assert!(
        !ran(&marker),
        "check ran a Python expression without the flag"
    );
    let o = run(&[
        "check",
        "--gpu",
        "--frames",
        "20",
        "--allow-python-ops",
        dir_s,
    ]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("not evaluated"), "{}", text(&o));
    assert!(ran(&marker), "check --allow-python-ops ran it");

    // render: the output needs the expression, so it refuses and says why.
    let o = run(&["render", dir_s, "--out", out_s, "--range", "0-3"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains(lsr_core::expr::PYTHON_NOT_ALLOWED),
        "{}",
        text(&o)
    );
    assert!(
        !ran(&marker),
        "render ran a Python expression without the flag"
    );
    let o = run(&[
        "render",
        dir_s,
        "--out",
        out_s,
        "--range",
        "0-3",
        "--allow-python-ops",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(ran(&marker), "render --allow-python-ops ran it");

    // serve: cooks the show, but not the project's Python.
    let serve = |allow: bool| {
        let mut args = vec![
            "serve",
            dir_s,
            "--http",
            "127.0.0.1:0",
            "--no-save",
            "--seconds",
            "1.5",
        ];
        if allow {
            args.push("--allow-python-ops");
        }
        run(&args)
    };
    let o = serve(false);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        !ran(&marker),
        "serve ran a Python expression without the flag"
    );
    let o = serve(true);
    assert!(o.status.success(), "{}", text(&o));
    assert!(ran(&marker), "serve --allow-python-ops ran it");

    std::fs::remove_dir_all(&root).ok();
}
