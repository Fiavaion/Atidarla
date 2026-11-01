//! Analyze on the GPU: reductions read back asynchronously.

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use serde_json::{Value, json};
use std::sync::{Mutex, MutexGuard};

/// One test at a time: two software (WARP) devices cooking compute passes in
/// parallel crashed or stalled their readbacks (9 of 25 local runs; CI runs
/// 39073127218 and 39143168336).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn engine(w: u32, h: u32) -> Option<Engine> {
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
        width: w,
        height: h,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name("n");
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty, name),
    }];
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits).unwrap();
    id
}

fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

/// Cooks until no readback is in flight; returns the channels.
fn measure(e: &mut Engine, node: NodeId) -> Vec<(String, f32)> {
    let start = std::time::Instant::now();
    let mut cooks = 0;
    loop {
        e.cook(&[node], 0.0).unwrap();
        cooks += 1;
        if cooks > 1 && !e.pending() {
            break;
        }
        // Generous: software adapters (WARP) compile and run slowly.
        assert!(
            start.elapsed().as_secs() < 60,
            "readback finished (error: {:?})",
            e.error(node)
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(e.error(node), None);
    let ch = e.channels(node).unwrap();
    ch.names()
        .map(|n| (n.to_owned(), ch.value(n).unwrap()))
        .collect()
}

fn get(values: &[(String, f32)], name: &str) -> f32 {
    values.iter().find(|(n, _)| n == name).unwrap().1
}

fn require_gpu() {
    let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(
        !required,
        "LSR_REQUIRE_GPU is set but there is no GPU adapter"
    );
}

#[test]
fn constant_and_ramp_measure_correctly() {
    let _serial = serial();
    // Not a multiple of the workgroup stride, to exercise the edges.
    let Some(mut e) = engine(333, 77) else {
        return require_gpu();
    };
    let c = add(
        &mut e,
        "img.constant",
        &[("colour", json!([0.2, 0.4, 0.6, 1.0]))],
    );
    let a = add(&mut e, "img.analyze", &[]);
    wire(&mut e, c, a, "in");
    let v = measure(&mut e, a);
    let names: Vec<&str> = v.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["r", "g", "b", "a", "luma", "min", "max"]);
    // 8-bit storage: 0.2 → 51/255 and so on.
    let q = |x: f32| (x * 255.0).round() / 255.0;
    for (n, want) in [("r", q(0.2)), ("g", q(0.4)), ("b", q(0.6)), ("a", 1.0)] {
        assert!((get(&v, n) - want).abs() < 1e-4, "{n}: {}", get(&v, n));
    }
    let luma = 0.2126 * q(0.2) + 0.7152 * q(0.4) + 0.0722 * q(0.6);
    for n in ["luma", "min", "max"] {
        assert!((get(&v, n) - luma).abs() < 1e-4, "{n}: {}", get(&v, n));
    }

    // A horizontal black→white ramp: average ½, range 0…1.
    let r = add(&mut e, "img.ramp", &[]);
    wire(&mut e, r, a, "in");
    let v = measure(&mut e, a);
    assert!(
        (get(&v, "luma") - 0.5).abs() < 0.01,
        "luma {}",
        get(&v, "luma")
    );
    assert!(get(&v, "min") < 0.01 && get(&v, "max") > 0.99);
    assert!((get(&v, "r") - get(&v, "g")).abs() < 1e-4);
}

#[test]
fn analysis_drives_a_parameter() {
    let _serial = serial();
    let Some(mut e) = engine(64, 32) else {
        return require_gpu();
    };
    // Grey 0.25 measured, then fed to Level's brightness over white.
    let grey = add(
        &mut e,
        "img.constant",
        &[("colour", json!([0.25, 0.25, 0.25, 1.0]))],
    );
    let a = add(&mut e, "img.analyze", &[]);
    wire(&mut e, grey, a, "in");
    let white = add(
        &mut e,
        "img.constant",
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let level = add(&mut e, "img.level", &[]);
    wire(&mut e, white, level, "in");
    e.edit(
        "bind",
        [Edit::SetParam {
            node: level,
            param: "brightness".into(),
            value: Some(Param::bound(json!(0.0), a, "luma")),
        }],
    )
    .unwrap();
    let start = std::time::Instant::now();
    let px = loop {
        e.cook(&[level], 0.0).unwrap();
        let img =
            lsr_interop::readback::read_rgba8(e.device(), e.queue(), e.result(level).unwrap())
                .unwrap();
        if img[0] > 0 && !e.pending() {
            break img[0];
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "measurement reached the parameter (analyze error: {:?})",
            e.error(a)
        );
    };
    assert!(px.abs_diff(64) <= 2, "white × measured 0.25 ≈ 64, got {px}");
}
