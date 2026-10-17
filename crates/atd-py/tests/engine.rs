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
