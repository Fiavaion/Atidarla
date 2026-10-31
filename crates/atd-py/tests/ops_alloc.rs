//! CLAUDE.md rule 2 for Python operators: on the cook thread a Python SIG
//! node allocates no more than a built-in SIG node once warm (its job and
//! results are copied in place; Python runs on the worker thread, whose
//! allocations are not counted).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_py::ops::PythonOps;
use serde_json::json;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Only the cook thread is measured.
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn counted() -> bool {
    COUNTING.try_with(std::cell::Cell::get).unwrap_or(false)
}

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counted() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counted() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const OPS: &str = r#"import atidarla as atd

@atd.op(
    family="SIG",
    params=[atd.Float("rate", default=2.0), atd.Menu("shape", options=("a", "b")),
            atd.String("tag", default="x"), atd.Vec("v", default=(1.0, 2.0, 3.0))],
    inputs=[atd.Input("in")],
    state={"n": 0.0},
)
def busy(ctx):
    ctx.state.n += ctx.par.rate
    return {"n": ctx.state.n, "in": ctx.inputs["in"].get("chan1", 0.0), "len": len(ctx.par.tag)}
"#;

/// Cook-thread allocations over `frames` live frames of `node`, after
/// waiting until its results are arriving.
fn measure(e: &mut Engine, node: NodeId, frames: usize) -> usize {
    let mut t = 0.0;
    let start = Instant::now();
    loop {
        e.cook(&[node], t).unwrap();
        t += 1.0 / 60.0;
        if e.channels(node).is_some_and(|c| !c.is_empty()) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "results arrive");
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..30 {
        e.cook(&[node], t).unwrap();
        t += 1.0 / 60.0;
        std::thread::sleep(Duration::from_millis(2));
    }
    COUNTING.with(|c| c.set(true));
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..frames {
        e.cook(&[node], t).unwrap();
        t += 1.0 / 60.0;
    }
    let n = ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.with(|c| c.set(false));
    assert_eq!(e.error(node), None);
    n
}

#[test]
fn python_sig_cooks_allocate_no_more_than_builtin_ones() {
    let instance = atd_gpu::primary_instance();
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
    let dir = std::env::temp_dir().join(format!("atd-pyops-alloc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ops")).unwrap();
    std::fs::write(dir.join("ops/busy.py"), OPS).unwrap();
    let mut ops = PythonOps::open(&dir);
    ops.allow();
    let mut reg = Registry::default();
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_sig::library(&mut lib);
    ops.register(&mut reg, &mut lib);
    let config = EngineConfig {
        width: 8,
        height: 8,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let (src, py, builtin) = (e.alloc_id(), e.alloc_id(), e.alloc_id());
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(src, "sig.lfo", "src"),
            },
            Edit::AddNode {
                node: Node::new(py, "py.busy.busy", "busy1"),
            },
            Edit::AddNode {
                node: Node::new(builtin, "sig.math", "math1"),
            },
            Edit::Connect {
                wire: atd_core::graph::Wire::new(
                    atd_core::graph::Endpoint::new(src, "out"),
                    atd_core::graph::Endpoint::new(py, "in"),
                ),
            },
            Edit::Connect {
                wire: atd_core::graph::Wire::new(
                    atd_core::graph::Endpoint::new(src, "out"),
                    atd_core::graph::Endpoint::new(builtin, "a"),
                ),
            },
            // A changing value through a binding, as a patch would have.
            Edit::SetParam {
                node: py,
                param: "rate".into(),
                value: Some(Param::bound(json!(0.0), src, "lfo")),
            },
        ],
    )
    .unwrap();
    let frames = 200;
    // Under load the worker's results arrive unevenly, which can add a few
    // one-time allocations to one run: noise only adds, so the least of a
    // few runs is the cook's own count.
    let (mut python, mut reference) = (usize::MAX, usize::MAX);
    for _ in 0..3 {
        python = python.min(measure(&mut e, py, frames));
        reference = reference.min(measure(&mut e, builtin, frames));
    }
    eprintln!(
        "cook-thread allocations over {frames} frames: Python SIG node {python}, built-in SIG node {reference}"
    );
    // A few one-time allocations may differ (the macOS runner counted 5215
    // against 5214, the Windows runner 5071 against 5000); a per-frame
    // allocation would add at least `frames`.
    assert!(
        python <= reference + frames / 2,
        "a Python SIG node allocated {python} times, a built-in one {reference}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
