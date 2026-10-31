//! Python expressions are project code (docs/python-ops.md): none reaches
//! the worker until the project's Python is allowed. Until then the
//! parameter keeps its stored value and the node says why; native
//! expressions keep working. One test, in its own process: the allowance is
//! process-wide.

use std::path::Path;
use std::time::{Duration, Instant};

use atd_core::expr::{PYTHON_NOT_ALLOWED, PyJob, PythonHost};
use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
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

#[test]
fn python_expressions_wait_for_the_projects_allowance() {
    assert!(atd_core::expr::Expr::parse(NATIVE).is_ok());
    assert!(atd_core::expr::Expr::parse(&writes(Path::new("x"))).is_err());
    // A project opens: its Python waits for the user's choice.
    atd_py::script::allow(false);

    // The host refuses jobs on its own too: nothing is evaluated.
    let marker = std::env::temp_dir().join(format!("atd-expr-trust-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let mut host = PyExprHost::start().unwrap();
    assert!(!host.allowed());
    let job = PyJob {
        key: 1,
        source: writes(&marker).into(),
        ..PyJob::default()
    };
    host.submit(std::slice::from_ref(&job));
    host.submit_wait(std::slice::from_ref(&job));
    std::thread::sleep(Duration::from_millis(300));
    let mut out = Vec::new();
    host.collect(&mut out);
    assert!(out.is_empty(), "{out:?}");
    assert!(!marker.exists(), "a refused job ran");
    drop(host);

    let Some(mut e) = engine() else { return };
    e.set_python(Box::new(PyExprHost::start().unwrap()));
    let (white, py, native) = (e.alloc_id(), e.alloc_id(), e.alloc_id());
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(white, "img.constant", "white"),
            },
            Edit::AddNode {
                node: Node::new(py, "img.level", "level1"),
            },
            Edit::AddNode {
                node: Node::new(native, "img.level", "level2"),
            },
            Edit::SetParam {
                node: white,
                param: "colour".into(),
                value: Some(Param::constant(json!([1.0, 1.0, 1.0, 1.0]))),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(white, "out"), Endpoint::new(py, "in")),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(white, "out"), Endpoint::new(native, "in")),
            },
            Edit::SetParam {
                node: py,
                param: "brightness".into(),
                value: Some(Param {
                    value: json!(1.0),
                    expr: Some(writes(&marker)),
                    bind: None,
                }),
            },
            Edit::SetParam {
                node: native,
                param: "brightness".into(),
                value: Some(Param {
                    value: json!(1.0),
                    expr: Some(NATIVE.into()),
                    bind: None,
                }),
            },
        ],
    )
    .unwrap();

    // Not allowed: the stored value, a clear error, nothing ran. The
    // native expression is unaffected.
    let mut t = 0.0;
    frames(&mut e, &[py, native], &mut t, Duration::from_millis(500));
    assert_eq!(red(&mut e, py, t), 255, "the stored brightness");
    let err = e.error(py).expect("an error on the node");
    assert!(err.contains(PYTHON_NOT_ALLOWED), "{err}");
    assert!(err.contains("brightness"), "{err}");
    assert!(matches!(red(&mut e, native, t), 127 | 128));
    assert_eq!(e.error(native), None);
    assert!(!marker.exists(), "the Python expression ran before allow");

    // Allowed: it runs, a frame or so late.
    atd_py::script::allow(true);
    let start = Instant::now();
    loop {
        t += 1.0 / 60.0;
        if red(&mut e, py, t) == 64 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the result arrives"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(e.error(py), None);
    assert!(marker.exists(), "the allowed expression ran");

    // Another project opens: the allowance resets, nothing runs again.
    atd_py::script::allow(false);
    // A batch already offered may still be with the worker; the worker
    // refuses it now, so the marker cannot come back after this point.
    frames(&mut e, &[py, native], &mut t, Duration::from_millis(200));
    std::fs::remove_file(&marker).unwrap();
    frames(&mut e, &[py, native], &mut t, Duration::from_millis(500));
    assert_eq!(red(&mut e, py, t), 255);
    assert!(e.error(py).is_some_and(|m| m.contains(PYTHON_NOT_ALLOWED)));
    assert!(matches!(red(&mut e, native, t), 127 | 128));
    assert!(!marker.exists(), "ran after the allowance was withdrawn");
}
