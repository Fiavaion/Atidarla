//! Python expressions in the engine: evaluated on the worker thread, used a
//! frame (or a few) later, never waited for.

use std::time::{Duration, Instant};

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
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
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

#[test]
fn python_expressions_arrive_a_frame_late_without_blocking() {
    let Some(mut e) = engine() else { return };
    e.set_python(Box::new(PyExprHost::start().unwrap()));
    let (white, level, knobs) = (e.alloc_id(), e.alloc_id(), e.alloc_id());
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(white, "img.constant", "white"),
            },
            Edit::AddNode {
                node: Node::new(level, "img.level", "level2"),
            },
            Edit::AddNode {
                node: Node::new(knobs, "sig.constant", "knobs"),
            },
            Edit::SetParam {
                node: white,
                param: "colour".into(),
                value: Some(Param::constant(json!([1.0, 1.0, 1.0, 1.0]))),
            },
            Edit::SetParam {
                node: knobs,
                param: "value1".into(),
                value: Some(Param::constant(json!(0.5))),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(white, "out"), Endpoint::new(level, "in")),
            },
            // Python only: a list, len() and a channel through op().
            Edit::SetParam {
                node: level,
                param: "brightness".into(),
                value: Some(Param {
                    value: json!(1.0),
                    expr: Some(
                        "[0.25, 0.5][me.digits - 1] * len('ab') * op('knobs')['chan1']".into(),
                    ),
                    bind: None,
                }),
            },
        ],
    )
    .unwrap();
    // The first frame cannot have a result yet: the stored value shows.
    let start = Instant::now();
    assert_eq!(red(&mut e, level, 0.0), 255);
    let elapsed = start.elapsed();
    let mut t = 0.0;
    loop {
        t += 1.0 / 60.0;
        // 0.5 is 127.5 in 8 bits: NVIDIA rounds it down, Metal and WARP up.
        if matches!(red(&mut e, level, t), 127 | 128) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the result arrives"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(e.error(level), None);
    eprintln!(
        "first frame took {elapsed:?}; result after {:.0} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );

    // An error from Python shows on the node; the stored value is used.
    e.edit(
        "expr",
        [Edit::SetParam {
            node: level,
            param: "brightness".into(),
            value: Some(Param {
                value: json!(0.25),
                expr: Some("{}['missing']".into()),
                bind: None,
            }),
        }],
    )
    .unwrap();
    loop {
        t += 1.0 / 60.0;
        red(&mut e, level, t);
        if e.error(level).is_some_and(|m| m.contains("KeyError")) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the error arrives"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(red(&mut e, level, t + 0.1), 64);
}

/// Offline (FR-H1) the cook waits for the worker: a Python result is
/// always exactly one frame late, however busy or slow the worker is.
#[test]
fn offline_python_results_are_exactly_one_frame_late() {
    let Some(mut e) = engine() else { return };
    e.set_python(Box::new(PyExprHost::start().unwrap()));
    e.set_mode(atd_gpu::engine::CookMode::Offline { fps: 60.0 });
    let (white, level) = (e.alloc_id(), e.alloc_id());
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(white, "img.constant", "white"),
            },
            Edit::AddNode {
                node: Node::new(level, "img.level", "level1"),
            },
            Edit::SetParam {
                node: white,
                param: "colour".into(),
                value: Some(Param::constant(json!([1.0, 1.0, 1.0, 1.0]))),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(white, "out"), Endpoint::new(level, "in")),
            },
            // Python only (a list); slow on purpose, and `frame` shows
            // which frame the value was computed for: 0.25 after frame 0,
            // 0.5 after frame 1.
            Edit::SetParam {
                node: level,
                param: "brightness".into(),
                value: Some(Param {
                    value: json!(1.0),
                    expr: Some("[__import__('time').sleep(0.05), 0.25 * (frame + 1)][1]".into()),
                    bind: None,
                }),
            },
        ],
    )
    .unwrap();
    // Frame 0 has no result yet (the stored value); each later frame has
    // the previous frame's.
    assert_eq!(red(&mut e, level, 0.0), 255);
    assert_eq!(red(&mut e, level, 1.0 / 60.0), 64);
    assert!(matches!(red(&mut e, level, 2.0 / 60.0), 127 | 128));
    assert_eq!(e.error(level), None);
}

/// PRD A66: a Python tuple drives a colour component by component (red,
/// green, blue; the stored alpha stays); a tuple of another length, or one
/// for a number, is an error and the stored value is used.
#[test]
fn python_tuples_drive_vectors_one_component_each() {
    let Some(mut e) = engine() else { return };
    e.set_python(Box::new(PyExprHost::start().unwrap()));
    e.set_mode(atd_gpu::engine::CookMode::Offline { fps: 60.0 });
    let (colour, level) = (e.alloc_id(), e.alloc_id());
    let expr = |node: NodeId, param: &str, value: serde_json::Value, source: &str| Edit::SetParam {
        node,
        param: param.into(),
        value: Some(Param {
            value,
            expr: Some(source.into()),
            bind: None,
        }),
    };
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(colour, "img.constant", "colour1"),
            },
            Edit::AddNode {
                node: Node::new(level, "img.level", "level1"),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(colour, "out"), Endpoint::new(level, "in")),
            },
            expr(
                colour,
                "colour",
                json!([0.0, 0.0, 0.0, 1.0]),
                "tuple([1, 0.5, 0][i] for i in range(3))",
            ),
        ],
    )
    .unwrap();
    let pixel = |e: &mut Engine, node: NodeId, f: f64| {
        e.cook(&[node], f / 60.0).unwrap();
        let px = read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap();
        [px[0], px[1], px[2], px[3]]
    };
    assert_eq!(pixel(&mut e, colour, 0.0), [0, 0, 0, 255], "stored first");
    let [r, g, b, a] = pixel(&mut e, colour, 1.0);
    assert_eq!((r, b, a), (255, 0, 255));
    assert!(matches!(g, 127 | 128), "{g}");
    assert_eq!(e.error(colour), None);
    e.edit(
        "bad",
        [
            expr(colour, "colour", json!([0.0, 0.0, 0.0, 1.0]), "(1, 0.5)"),
            expr(level, "brightness", json!(0.25), "(1, 2)"),
        ],
    )
    .unwrap();
    pixel(&mut e, level, 2.0);
    assert_eq!(pixel(&mut e, level, 3.0), [0, 0, 0, 255], "stored values");
    assert_eq!(
        e.error(colour),
        Some("expression for `colour`: gives 2 numbers; `colour` takes one, or 3")
    );
    assert_eq!(
        e.error(level),
        Some("expression for `brightness`: gives 2 numbers; `brightness` takes one")
    );
}

/// Offline, a Python expression that does not finish is a node error after
/// the timeout, not a hung render.
#[test]
fn offline_waits_for_python_expressions_are_bounded() {
    let Some(mut e) = engine() else { return };
    let mut host = PyExprHost::start().unwrap();
    host.set_offline_timeout(Duration::from_millis(300));
    e.set_python(Box::new(host));
    e.set_mode(atd_gpu::engine::CookMode::Offline { fps: 60.0 });
    let (white, level) = (e.alloc_id(), e.alloc_id());
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(white, "img.constant", "white"),
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
                    expr: Some("[__import__('time').sleep(3), 0.5][1]".into()),
                    bind: None,
                }),
            },
        ],
    )
    .unwrap();
    let start = Instant::now();
    red(&mut e, level, 0.0);
    red(&mut e, level, 1.0 / 60.0);
    let took = start.elapsed();
    let msg = e.error(level).unwrap_or_default();
    assert!(msg.contains("TimeoutError"), "{msg}");
    assert!(took < Duration::from_secs(2), "{took:?}");
}
