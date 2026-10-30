//! Rule 2 for project kernel operators (`kernels/*.atk`): once compiled,
//! frames create no buffers, bind groups or textures, and reading the
//! node's `ch()` values (one driven by an expression, so the uniforms change
//! every frame) adds no allocations beyond wgpu's own per pass. The bound is
//! the one of `tests/alloc.rs`, per node-cook.
//!
//! One test in this binary: the bind-group counter is process-wide.

use std::alloc::{GlobalAlloc, Layout, System};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::kernels::bind_groups_created_total;
use atd_ops_pnt::project::ProjectKernels;
use serde_json::json;

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

/// Allocations per node-cook allowed (as `tests/alloc.rs`).
const BOUND: f64 = 90.0;

#[test]
fn project_kernels_steady_state_allocations() {
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
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .unwrap();
    let dir = std::env::temp_dir().join(format!("atd-kernels-alloc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("kernels")).unwrap();
    std::fs::write(
        dir.join("kernels/wave.atk"),
        "float amp = ch(\"amp\", 0.1);\nint n = chi(\"n\", 3);\nvec3 dir = chv(\"dir\", {0, 1, 0});\n\
         @P += dir * sin(@P.x * float(n) + @Time) * amp * @TimeInc;\nf@age += @TimeInc;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("kernels/tint.pixels.atk"),
        "vec3 rgb = chv(\"rgb\", {1, 0.5, 0.25});\n@Cd = vec4(@Cd.rgb * rgb * ch(\"gain\", 1.0), 1);\n",
    )
    .unwrap();
    let kernels = ProjectKernels::open(&dir);
    assert!(kernels.errors().is_empty(), "{:?}", kernels.errors());
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    atd_ops_img::library(&mut lib);
    kernels.register(&mut reg, &mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    // Offline, so the first cook waits for the compiles.
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let mut edits = Vec::new();
    let mut node = |e: &mut Engine, ty: &str| {
        let id = e.alloc_id();
        edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", id.0)),
        });
        id
    };
    let pts = node(&mut e, "pnt.points");
    let wave = node(&mut e, "kernel.wave");
    let draw = node(&mut e, "img.render_points");
    let tint = node(&mut e, "kernel.tint");
    let wire = |from: NodeId, to: NodeId, socket: &str| Edit::Connect {
        wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
    };
    edits.extend([
        wire(pts, wave, "in"),
        wire(wave, draw, "geo"),
        wire(draw, tint, "in0"),
        Edit::SetParam {
            node: pts,
            param: "count".into(),
            value: Some(Param::constant(json!(4096))),
        },
        // Changes every frame: the uniforms are written every frame.
        Edit::SetParam {
            node: wave,
            param: "amp".into(),
            value: Some(Param {
                value: json!(0.0),
                expr: Some("0.1 + 0.05 * sin(time)".into()),
                bind: None,
            }),
        },
        Edit::SetParam {
            node: wave,
            param: "dir".into(),
            value: Some(Param::constant(json!([0.0, 1.0, 0.5]))),
        },
        Edit::SetParam {
            node: tint,
            param: "gain".into(),
            value: Some(Param {
                value: json!(0.0),
                expr: Some("0.8 + 0.2 * sin(time)".into()),
                bind: None,
            }),
        },
    ]);
    e.edit("chain", edits).unwrap();
    let out = [tint];
    let mut t = 0.0;
    for _ in 0..30 {
        e.cook(&out, t).unwrap();
        t += 1.0 / 60.0;
    }
    for n in [pts, wave, draw, tint] {
        assert_eq!(e.error(n), None);
    }
    let (buffers, binds, textures) = (
        e.buffer_pool().created(),
        bind_groups_created_total(),
        e.pool().created(),
    );
    let frames = 100;
    COUNTING.with(|c| c.set(true));
    let before = allocs();
    for _ in 0..frames {
        let cooked = e.cook(&out, t).unwrap();
        assert_eq!(cooked, 3, "everything but the static source cooks");
        t += 1.0 / 60.0;
    }
    let total = allocs() - before;
    COUNTING.with(|c| c.set(false));
    let per_node = total as f64 / (frames * 3) as f64;
    eprintln!(
        "project kernels steady state: {total} allocations over {frames} frames, {per_node:.1} per node-cook"
    );
    assert_eq!(e.buffer_pool().created(), buffers, "no buffers created");
    assert_eq!(bind_groups_created_total(), binds, "no bind groups created");
    assert_eq!(e.pool().created(), textures, "no textures created");
    assert!(
        per_node <= BOUND,
        "{per_node:.1} allocations per node-cook (bound {BOUND})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
