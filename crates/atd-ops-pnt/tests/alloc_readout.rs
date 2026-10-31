//! Rule 2 for the GPU→SIG point readbacks (To Channels, Pixel Out; PRD
//! A65): after warm-up, animated points read back every frame create no
//! pooled buffers or textures, and the render thread's allocations per
//! node-cook stay within the bound of the other PNT tests (wgpu allocates
//! per pass, per `write_buffer` and per `map_async`, so it is not zero).
//!
//! One test in this binary: the allocation counter is per thread but the
//! GPU is shared.

use std::alloc::{GlobalAlloc, Layout, System};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use serde_json::{Value, json};

struct Counting;
thread_local! {
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn allocs() -> usize {
    ALLOCS.with(std::cell::Cell::get)
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations per node-cook allowed: the PNT batch's bound
/// (`tests/alloc_batch.rs`). Rebuilding 192 channel names, or a staging
/// buffer, every frame would exceed it many times over.
const BOUND: f64 = 110.0;

const PATCH: &str = r#"
[[fixture]]
id = "strip"
type = "pixel-grb"
universe = 1
count = 64
line = [[0.0, 0.5], [1.0, 0.5]]
"#;

fn node(e: &mut Engine, edits: &mut Vec<Edit>, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    edits.push(Edit::AddNode {
        node: Node::new(id, ty, format!("n{}", id.0)),
    });
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    id
}

#[test]
fn point_readbacks_steady_state_allocations() {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .unwrap();
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: 16,
        height: 16,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let dir = std::env::temp_dir().join(format!("atd-alloc-readout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("patch.toml"), PATCH).unwrap();
    e.set_project_dir(Some(dir.clone()));

    let mut edits = Vec::new();
    let pts = node(
        &mut e,
        &mut edits,
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(64)),
            ("random_colour", json!(true)),
        ],
    );
    edits.push(Edit::SetParam {
        node: pts,
        param: "seed".into(),
        value: Some(Param {
            value: json!(1),
            expr: Some("1 + frame".into()),
            bind: None,
        }),
    });
    let per_point = node(&mut e, &mut edits, "pnt.to_channels", &[("max", json!(64))]);
    let samples = node(
        &mut e,
        &mut edits,
        "pnt.to_channels",
        &[("attr", json!("Cd")), ("layout", json!("samples"))],
    );
    let pixels = node(
        &mut e,
        &mut edits,
        "pnt.pixel_out",
        &[("patch", json!("patch.toml"))],
    );
    for to in [per_point, samples, pixels] {
        edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(pts, "out"), Endpoint::new(to, "in")),
        });
    }
    e.edit("readouts", edits).unwrap();
    let out = [per_point, samples, pixels];
    let mut t = 0.0;
    let start = std::time::Instant::now();
    // Warm up: the patch file's watcher, the staging buffers, the names.
    while e.channels(pixels).is_none_or(|c| c.is_empty()) || t < 1.0 {
        e.cook(&out, t).unwrap();
        t += 1.0 / 60.0;
        assert!(start.elapsed().as_secs() < 60, "warmed up");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    for n in [pts, per_point, samples, pixels] {
        assert_eq!(e.error(n), None);
    }
    assert_eq!(e.channels(per_point).unwrap().len(), 192);
    assert_eq!(e.channels(pixels).unwrap().len(), 192);
    let (buffers, textures) = (e.buffer_pool().created(), e.pool().created());
    let first = e.channels(per_point).unwrap().clone();
    let frames = 100;
    COUNTING.with(|c| c.set(true));
    let before = allocs();
    let mut cooked = 0;
    for _ in 0..frames {
        cooked += e.cook(&out, t).unwrap();
        t += 1.0 / 60.0;
    }
    let total = allocs() - before;
    COUNTING.with(|c| c.set(false));
    let per_node = total as f64 / cooked as f64;
    eprintln!(
        "point readbacks steady state: {total} allocations over {frames} frames, {cooked} node-cooks, {per_node:.1} per node-cook"
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_ne!(
        e.channels(per_point).unwrap(),
        &first,
        "new points were read back while measuring"
    );
    assert_eq!(e.buffer_pool().created(), buffers, "no buffers created");
    assert_eq!(e.pool().created(), textures, "no textures created");
    assert!(
        per_node <= BOUND,
        "{per_node:.1} allocations per node-cook (bound {BOUND})"
    );
}
