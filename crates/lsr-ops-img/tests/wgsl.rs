//! The WGSL operator: inline code, parameters, errors that keep the last
//! good shader, and a file that reloads when saved.

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_interop::readback::read_rgba8;
use serde_json::{Value, json};

fn engine() -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: 16,
        height: 8,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn set(e: &mut Engine, node: NodeId, p: &str, v: Value) {
    e.edit(
        "set",
        [Edit::SetParam {
            node,
            param: p.into(),
            value: Some(Param::constant(v)),
        }],
    )
    .unwrap();
}

/// Cooks at time `t` until compilation settles; returns pixel (0, 0).
fn settle(
    e: &mut Engine,
    node: NodeId,
    t: f64,
    until: impl Fn(&Engine, [u8; 4]) -> bool,
) -> [u8; 4] {
    let start = std::time::Instant::now();
    loop {
        e.cook(&[node], t).unwrap();
        let px = read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap();
        let px: [u8; 4] = px[..4].try_into().unwrap();
        if until(e, px) {
            return px;
        }
        assert!(
            start.elapsed().as_secs() < 30,
            "settled: {px:?}, error {:?}",
            e.error(node)
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn inline_code_params_errors_and_file_reload() {
    let Some(mut e) = engine() else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let red = e.alloc_id();
    let w = e.alloc_id();
    e.edit(
        "add",
        [
            Edit::AddNode {
                node: Node::new(red, "img.constant", "red"),
            },
            Edit::SetParam {
                node: red,
                param: "colour".into(),
                value: Some(Param::constant(json!([1.0, 0.0, 0.0, 1.0]))),
            },
            Edit::AddNode {
                node: Node::new(w, "img.wgsl", "w"),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(red, "out"), Endpoint::new(w, "in0")),
            },
        ],
    )
    .unwrap();

    // The default passes `in0` through.
    let px = settle(&mut e, w, 0.0, |_, px| px == [255, 0, 0, 255]);
    assert_eq!(px, [255, 0, 0, 255]);

    // Parameters and time.
    set(&mut e, w, "code", json!("return vec4f(p1().xyz, 1.0);"));
    set(&mut e, w, "p1", json!([0.0, 1.0, 0.0, 0.0]));
    settle(&mut e, w, 0.0, |_, px| px == [0, 255, 0, 255]);
    set(
        &mut e,
        w,
        "code",
        json!("return vec4f(fract(time()), 0.0, 0.0, 1.0);"),
    );
    let px = settle(&mut e, w, 2.5, |_, px| px[1] == 0);
    assert!(px[0].abs_diff(128) <= 1, "{px:?}");

    // A syntax error names the line; the last good shader keeps running.
    set(&mut e, w, "code", json!("let x = 1.0;\nreturn vec4f(x;"));
    let px = settle(&mut e, w, 2.5, |e, _| e.error(w).is_some());
    let err = e.error(w).unwrap().to_owned();
    assert!(err.starts_with("line 2:"), "{err}");
    assert!(
        px[0].abs_diff(128) <= 1,
        "previous shader still drawn: {px:?}"
    );
    // So does a type error found by validation.
    set(&mut e, w, "code", json!("return 1.0;"));
    settle(&mut e, w, 2.5, |e, _| {
        e.error(w).is_some_and(|m| m.starts_with("line 1:"))
    });

    // A file reloads when it changes.
    let path = std::env::temp_dir().join(format!("lsr-wgsl-{}.wgsl", std::process::id()));
    std::fs::write(&path, "return vec4f(0.0, 0.0, 1.0, 1.0);").unwrap();
    set(&mut e, w, "file", json!(path.to_string_lossy()));
    settle(&mut e, w, 0.0, |e, px| {
        px == [0, 0, 255, 255] && e.error(w).is_none()
    });
    std::fs::write(&path, "return vec4f(1.0, 1.0, 0.0, 1.0);").unwrap();
    settle(&mut e, w, 0.0, |_, px| px == [255, 255, 0, 255]);
    std::fs::remove_file(&path).ok();
    settle(&mut e, w, 0.0, |e, _| {
        e.error(w).is_some_and(|m| m.contains("lsr-wgsl"))
    });
}
