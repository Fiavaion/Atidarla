//! Rule 2 for IMG cooks: steady-state frames create no GPU objects (bind
//! groups, views) and allocate little. wgpu itself allocates while
//! recording commands, so the test bounds allocations per node per frame
//! rather than demanding zero.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use serde_json::json;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn img_chain_steady_state_allocations() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);

    // A time-dependent source so every node cooks every frame.
    let chain = [
        "img.noise",
        "img.blur",
        "img.level",
        "img.hsv_adjust",
        "img.transform",
        "img.kaleidoscope",
        "img.mirror",
        "img.edge",
        "img.composite",
        "img.feedback",
    ];
    let mut ids: Vec<NodeId> = Vec::new();
    let mut edits = Vec::new();
    for ty in chain {
        let id = e.alloc_id();
        edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", ids.len())),
        });
        if let Some(prev) = ids.last() {
            let input = if ty == "img.composite" { "a" } else { "in" };
            edits.push(Edit::Connect {
                wire: Wire::new(Endpoint::new(*prev, "out"), Endpoint::new(id, input)),
            });
        }
        ids.push(id);
    }
    // An LFO on the noise's z makes the whole chain change every frame.
    let lfo = e.alloc_id();
    edits.push(Edit::AddNode {
        node: Node::new(lfo, "sig.lfo", "lfo1"),
    });
    edits.push(Edit::SetParam {
        node: ids[0],
        param: "z".into(),
        value: Some(Param::bound(json!(0.0), lfo, "lfo")),
    });
    e.edit("chain", edits).unwrap();
    let out = *ids.last().unwrap();

    let mut t = 0.0;
    for _ in 0..30 {
        e.cook(&[out], t).unwrap();
        t += 1.0 / 60.0;
    }
    let frames = 100;
    COUNTING.with(|c| c.set(true));
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..frames {
        let cooked = e.cook(&[out], t).unwrap();
        assert_eq!(cooked, ids.len() + 1, "every node cooks every frame");
        t += 1.0 / 60.0;
    }
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.with(|c| c.set(false));
    let per_node = allocs as f64 / (frames * ids.len()) as f64;
    eprintln!(
        "IMG steady state: {allocs} allocations over {frames} frames, {per_node:.1} per node-cook"
    );
    assert!(
        per_node <= BOUND,
        "{per_node:.1} allocations per node-cook (bound {BOUND})"
    );
}

/// Allocations per node-cook allowed: what remains is wgpu recording the
/// render pass (17.7 measured on lavapipe; 70 before bind-group caching).
const BOUND: f64 = 25.0;
