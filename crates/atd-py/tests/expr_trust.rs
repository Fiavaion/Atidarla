//! Python expressions are project code (docs/python-ops.md): one reaches
//! the worker only while its exact content (node, parameter, source) is on
//! the allow-list. Until then the parameter keeps its stored value and the
//! node says why; other allowed expressions and native ones keep working.
//! One test, in its own process: the list is process-wide.

use std::path::Path;
use std::time::{Duration, Instant};

use atd_core::expr::{PYTHON_NOT_ALLOWED, PyJob, PythonHost};
use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use atd_py::allow::{self, Item};
use atd_py::expr::PyExprHost;
use serde_json::json;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: 4,
        height: 4,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn red(e: &mut Engine, node: NodeId, time: f64) -> u8 {
    e.cook(&[node], time).unwrap();
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()[0]
}

/// Cooks frames (as the app does, a few ms apart) for `wait`.
fn frames(e: &mut Engine, nodes: &[NodeId], t: &mut f64, wait: Duration) {
    let start = Instant::now();
    while start.elapsed() < wait {
        *t += 1.0 / 60.0;
        e.cook(nodes, *t).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A Python expression that writes `marker` (as an untrusted project's
/// could do anything) and gives 0.25.
fn writes(marker: &Path) -> String {
    format!(
        "__import__('pathlib').Path({:?}).write_text('ran') * 0 + 0.25",
        marker.display().to_string()
    )
}

/// In the native subset: evaluated on the cook thread, no Python.
const NATIVE: &str = "0.5 + 0 * time";

fn level(e: &mut Engine, name: &str, white: NodeId, expr: &str) -> NodeId {
    let id = e.alloc_id();
    e.edit(
        "add",
        [
            Edit::AddNode {
                node: Node::new(id, "img.level", name),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(white, "out"), Endpoint::new(id, "in")),
            },
            Edit::SetParam {
                node: id,
                param: "brightness".into(),
                value: Some(Param {
                    value: json!(1.0),
                    expr: Some(expr.into()),
                    bind: None,
                }),
            },
        ],
    )
    .unwrap();
    id
}

fn set_expr(e: &mut Engine, node: NodeId, expr: &str) {
    e.edit(
        "expr",
        [Edit::SetParam {
            node,
            param: "brightness".into(),
            value: Some(Param {
                value: json!(1.0),
                expr: Some(expr.into()),
                bind: None,
            }),
        }],
    )
    .unwrap();
}

fn item(node: NodeId, expr: &str) -> Item {
    Item::Expr {
        node,
        param: "brightness".into(),
        source: expr.into(),
    }
}

/// Cooks until `node`'s red is `want` (20 s at most).
fn until_red(e: &mut Engine, node: NodeId, t: &mut f64, want: u8) {
    let start = Instant::now();
    loop {
        *t += 1.0 / 60.0;
        if red(e, node, *t) == want {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "red {want} arrives"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the marker was written since the last call (and removes it).
fn ran(marker: &Path) -> bool {
    let yes = marker.exists();
    let _ = std::fs::remove_file(marker);
    yes
}

#[test]
fn python_expressions_run_only_as_allowed() {
    assert!(atd_core::expr::Expr::parse(NATIVE).is_ok());
    assert!(atd_core::expr::Expr::parse(&writes(Path::new("x"))).is_err());
    let dir = std::env::temp_dir().join(format!("atd-expr-trust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (m1, m2) = (dir.join("own"), dir.join("foreign"));
    // A project opens: nothing is allowed.
    allow::clear();

    // The worker refuses a job that is not allowed, even when offered.
    let mut host = PyExprHost::start().unwrap();
    let job = PyJob {
        key: 1,
        source: writes(&m2).into(),
        ..PyJob::default()
    };
    assert!(!host.allows(job.node, &job.param, &job.source));
    host.submit_wait(std::slice::from_ref(&job));
    let mut out = Vec::new();
    host.collect(&mut out);
    assert!(
        matches!(&out[..], [(1, Err(m))] if m.contains(PYTHON_NOT_ALLOWED)),
        "{out:?}"
    );
    assert!(!ran(&m2), "a refused job ran");
    drop(host);

    let Some(mut e) = engine() else { return };
    e.set_python(Box::new(PyExprHost::start().unwrap()));
    let white = e.alloc_id();
    e.edit(
        "white",
        [
            Edit::AddNode {
                node: Node::new(white, "img.constant", "white"),
            },
            Edit::SetParam {
                node: white,
                param: "colour".into(),
                value: Some(Param::constant(json!([1.0, 1.0, 1.0, 1.0]))),
            },
        ],
    )
    .unwrap();
    let native = level(&mut e, "native1", white, NATIVE);
    let own_src = writes(&m1);
    let own = level(&mut e, "own1", white, &own_src);
    let mut t = 0.0;

    // Not allowed: the stored value, a clear error, nothing ran; the native
    // expression is unaffected.
    frames(&mut e, &[own, native], &mut t, Duration::from_millis(400));
    assert_eq!(red(&mut e, own, t), 255, "the stored brightness");
    let err = e.error(own).expect("an error on the node");
    assert!(
        err.contains(PYTHON_NOT_ALLOWED) && err.contains("brightness"),
        "{err}"
    );
    assert!(matches!(red(&mut e, native, t), 127 | 128));
    assert_eq!(e.error(native), None);
    assert!(!ran(&m1), "ran before it was allowed");

    // Allowed (the dialog): it runs, a frame or so late.
    allow::allow([item(own, &own_src)]);
    until_red(&mut e, own, &mut t, 64);
    assert_eq!(e.error(own), None);
    assert!(ran(&m1), "the allowed expression ran");

    // Foreign Python joins (a paste, a merge) and is cooked at once: it is
    // blocked from the first frame, and the allowed one keeps running.
    let foreign_src = writes(&m2);
    let foreign = level(&mut e, "foreign1", white, &foreign_src);
    for _ in 0..30 {
        t += 1.0 / 60.0;
        e.cook(&[own, foreign], t).unwrap();
    }
    frames(&mut e, &[own, foreign], &mut t, Duration::from_millis(300));
    assert!(!ran(&m2), "foreign Python ran");
    assert!(
        e.error(foreign)
            .is_some_and(|m| m.contains(PYTHON_NOT_ALLOWED))
    );
    assert_eq!(red(&mut e, own, t), 64, "the allowed one keeps running");
    assert!(ran(&m1));

    // A change after the first 60 characters is new content: blocked.
    let tail = format!("{own_src} + 0 * len('{}')", "x".repeat(80));
    set_expr(&mut e, own, &tail);
    frames(&mut e, &[own], &mut t, Duration::from_millis(300));
    let _ = ran(&m1);
    frames(&mut e, &[own], &mut t, Duration::from_millis(300));
    assert!(!ran(&m1), "the changed expression ran");
    assert!(e.error(own).is_some_and(|m| m.contains(PYTHON_NOT_ALLOWED)));
    assert_eq!(red(&mut e, own, t), 255);

    // Undo brings back the allowed content: it runs. Redo brings back the
    // changed one: blocked again.
    e.undo().unwrap();
    until_red(&mut e, own, &mut t, 64);
    e.redo().unwrap();
    until_red(&mut e, own, &mut t, 255);
    let _ = ran(&m1);
    frames(&mut e, &[own], &mut t, Duration::from_millis(300));
    assert!(!ran(&m1), "redo resurrected a blocked expression");

    // Undo the change and the foreign node's addition, redo the addition:
    // the foreign node is still not allowed.
    e.undo().unwrap();
    e.undo().unwrap();
    e.redo().unwrap();
    frames(&mut e, &[foreign], &mut t, Duration::from_millis(300));
    assert!(!ran(&m2), "undo/redo resurrected foreign Python");

    // Another project opens: the list is cleared, nothing runs.
    until_red(&mut e, own, &mut t, 64);
    allow::clear();
    frames(&mut e, &[own, native], &mut t, Duration::from_millis(200));
    let _ = ran(&m1);
    frames(&mut e, &[own, native], &mut t, Duration::from_millis(400));
    assert_eq!(red(&mut e, own, t), 255);
    assert!(e.error(own).is_some_and(|m| m.contains(PYTHON_NOT_ALLOWED)));
    assert!(matches!(red(&mut e, native, t), 127 | 128));
    assert!(!ran(&m1), "ran after the list was cleared");
    std::fs::remove_dir_all(&dir).ok();
}
