//! Rule 2 for ComfyUI Workflow nodes: while a prompt is in flight the node
//! cooks every frame to collect what arrives, and on frames where nothing
//! arrives it only draws the image it holds. The engine and wgpu allocate
//! for every frame and draw, so such a frame is compared with a frame of a
//! WGSL node drawing the same pass-through shader, rather than required to
//! allocate nothing.

mod mock_comfy;

use std::alloc::{GlobalAlloc, Layout, System};
use std::time::{Duration, Instant};

use lsr_core::graph::{Edit, Node, Param};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_ops_ai::comfy::{ProjectComfy, import};
use mock_comfy::MockComfy;
use serde_json::json;

struct Counting;
thread_local! {
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Per thread: the mock server's and the worker's threads do not count.
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

/// Allocations a waiting frame may make beyond the WGSL frame's.
const MARGIN: f64 = 2.0;

/// Average allocations per frame over `frames` cooks of `node` on this
/// thread.
fn per_frame(e: &mut Engine, node: lsr_core::graph::NodeId, t: &mut f64, frames: u32) -> f64 {
    COUNTING.with(|c| c.set(true));
    let before = allocs();
    for _ in 0..frames {
        *t += 1.0 / 60.0;
        let cooked = e.cook(&[node], *t).unwrap();
        assert_eq!(cooked, 1, "the node cooks every frame");
    }
    let n = allocs() - before;
    COUNTING.with(|c| c.set(false));
    n as f64 / f64::from(frames)
}

#[test]
fn frames_while_a_prompt_is_in_flight_allocate_little() {
    let mock = MockComfy::start();
    // The prompt stays in flight for the whole measurement.
    mock.configure(|c| c.node_delay = Duration::from_secs(3));
    let dir = std::env::temp_dir().join(format!("lsr-comfy-alloc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("project.toml"), "format = 1\nname = \"a\"\n").unwrap();
    let wf = dir.join("invert.json");
    std::fs::write(
        &wf,
        json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "2": {"class_type": "ImageInvert", "inputs": {"image": ["1", 0]}},
            "3": {"class_type": "SaveImage", "inputs": {"filename_prefix": "a", "images": ["2", 0]}}
        })
        .to_string(),
    )
    .unwrap();
    import(&wf, &dir, &mock.server(), None).unwrap();

    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    let comfy = ProjectComfy::open(&dir);
    comfy.register(&mut reg, &mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let node = e.alloc_id();
    e.edit(
        "add",
        [
            Edit::AddNode {
                node: Node::new(node, "comfy.invert", "invert1"),
            },
            Edit::SetParam {
                node,
                param: "server".into(),
                value: Some(Param::constant(json!(mock.server()))),
            },
            Edit::SetParam {
                node,
                param: "run".into(),
                value: Some(Param::constant(json!(true))),
            },
        ],
    )
    .unwrap();
    // The first cook sees Run; press it, then wait until the prompt runs.
    let mut t = 0.0;
    e.cook(&[node], t).unwrap();
    e.edit(
        "press",
        [Edit::SetParam {
            node,
            param: "run".into(),
            value: Some(Param::constant(json!(false))),
        }],
    )
    .unwrap();
    let start = Instant::now();
    while !e.status(node).unwrap_or("").starts_with("running") {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            e.status(node)
        );
        t += 1.0 / 60.0;
        e.cook(&[node], t).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    // A WGSL node drawing the same pass-through, warmed up (it compiles on
    // a worker thread).
    let wgsl = e.alloc_id();
    e.edit(
        "add",
        [Edit::AddNode {
            node: Node::new(wgsl, "img.wgsl", "wgsl1"),
        }],
    )
    .unwrap();
    let warm = Instant::now();
    while (e.status(wgsl).is_some() || e.pending()) && warm.elapsed() < Duration::from_secs(5) {
        t += 1.0 / 60.0;
        e.cook(&[wgsl], t).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..10 {
        t += 1.0 / 60.0;
        e.cook(&[wgsl, node], t).unwrap();
    }
    let baseline = per_frame(&mut e, wgsl, &mut t, 60);
    let waiting = per_frame(&mut e, node, &mut t, 60);
    assert!(e.status(node).unwrap().starts_with("running"));
    eprintln!("allocations per frame: ComfyUI node waiting {waiting:.1}, WGSL node {baseline:.1}");
    assert!(
        waiting <= baseline + MARGIN,
        "a waiting frame makes {waiting:.1} allocations, a WGSL frame {baseline:.1}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
