//! Rule 2 for the M2 PNT batch, as `tests/alloc.rs` does for the geometry
//! core: after warm-up, animated chains of the batch's operators create no
//! buffers, bind groups or textures, and the render thread's heap
//! allocations per node-cook stay bounded (wgpu allocates per pass and per
//! `write_buffer`, so the bound is not zero).
//!
//! One test in this binary: the bind-group counter is process-wide.

use std::alloc::{GlobalAlloc, Layout, System};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_gpu::geo::kernels::bind_groups_created_total;
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

/// Allocations per node-cook allowed. Measured 40.0 on NVIDIA DX12 and 87.6 on
/// the Metal runner in CI (wgpu records more per pass there); the geometry
/// core's bound is 90 (`tests/alloc.rs`). Per-frame buffers, bind groups or
/// textures would add far more, and those are counted separately above.
const BOUND: f64 = 110.0;

struct Net {
    edits: Vec<Edit>,
}

impl Net {
    fn node(&mut self, e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let id = e.alloc_id();
        self.edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", id.0)),
        });
        for (p, v) in params {
            self.edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
        id
    }

    fn animate(&mut self, node: NodeId, param: &str, expr: &str) {
        self.edits.push(Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param {
                value: json!(0.0),
                expr: Some(expr.into()),
                bind: None,
            }),
        });
    }

    fn wire(&mut self, from: NodeId, to: NodeId, socket: &str) {
        self.edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
        });
    }

    fn chain(
        &mut self,
        e: &mut Engine,
        from: NodeId,
        ty: &str,
        params: &[(&str, Value)],
    ) -> NodeId {
        let n = self.node(e, ty, params);
        self.wire(from, n, "in");
        n
    }
}

#[test]
fn batch_chains_steady_state_allocations() {
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
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: lsr_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .unwrap();
    let mut reg = Registry::default();
    lsr_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let mut n = Net { edits: Vec::new() };
    // 1. A particle loop with an emitter and forces, coloured by speed.
    let sys = n.node(
        &mut e,
        "pnt.particles",
        &[("capacity", json!(20_000)), ("rate", json!(0.0))],
    );
    let ring = n.node(
        &mut e,
        "pnt.scatter",
        &[("shape", json!("circle")), ("count", json!(500))],
    );
    let emit = n.chain(&mut e, sys, "pnt.emitter", &[("rate", json!(20_000.0))]);
    n.wire(ring, emit, "source");
    let grav = n.chain(&mut e, emit, "pnt.gravity", &[]);
    let vortex = n.chain(&mut e, grav, "pnt.vortex", &[]);
    let turb = n.chain(&mut e, vortex, "pnt.turbulence", &[]);
    n.animate(turb, "evolution", "time * 0.3");
    let floor = n.chain(&mut e, turb, "pnt.collide", &[]);
    n.wire(floor, sys, "prev");
    let colour = n.chain(
        &mut e,
        floor,
        "pnt.colour",
        &[("attribute", json!("v")), ("measure", json!("length"))],
    );
    let r1 = n.node(&mut e, "img.render_points", &[]);
    n.wire(colour, r1, "geo");
    // 2. A deformer stack with a cut and attributes.
    let column = n.node(
        &mut e,
        "pnt.scatter",
        &[("shape", json!("box")), ("count", json!(8192))],
    );
    let twist = n.chain(&mut e, column, "pnt.twist", &[]);
    n.animate(twist, "angle", "90 * sin(time)");
    let cut = n.chain(
        &mut e,
        twist,
        "pnt.delete",
        &[("radius", json!(0.3)), ("rule", json!("sphere"))],
    );
    let bend = n.chain(&mut e, cut, "pnt.bend", &[]);
    let wave = n.chain(&mut e, bend, "pnt.wave", &[]);
    n.animate(wave, "phase", "time * 90");
    let math = n.chain(
        &mut e,
        wave,
        "pnt.attribute_math",
        &[("a", json!("P")), ("operation", json!("multiply"))],
    );
    let rand = n.chain(&mut e, math, "pnt.randomize", &[]);
    let r2 = n.node(&mut e, "img.render_points", &[]);
    n.wire(rand, r2, "geo");
    // 3. Neighbours and a solver (three sub-steps a frame).
    let sphere = n.node(
        &mut e,
        "pnt.scatter",
        &[("shape", json!("sphere")), ("count", json!(2000))],
    );
    let flow = n.chain(&mut e, sphere, "pnt.noise", &[]);
    n.animate(flow, "evolution", "time * 0.2");
    let nrm = n.chain(&mut e, flow, "pnt.normals", &[]);
    let links = n.chain(&mut e, nrm, "pnt.connect", &[("radius", json!(0.1))]);
    let r3 = n.node(&mut e, "img.render_points", &[("draw", json!("both"))]);
    n.wire(links, r3, "geo");
    let solver = n.node(&mut e, "pnt.solver", &[("substeps", json!(3))]);
    n.wire(sphere, solver, "init");
    let pull = n.chain(&mut e, solver, "pnt.attract", &[]);
    let step = n.chain(&mut e, pull, "pnt.integrate", &[]);
    let age = n.chain(
        &mut e,
        step,
        "pnt.age",
        &[
            ("kill_dead", json!(false)),
            ("region", json!("outside_sphere")),
            ("radius", json!(3.0)),
        ],
    );
    n.wire(age, solver, "prev");
    let r4 = n.node(&mut e, "img.render_points", &[]);
    n.wire(age, r4, "geo");
    // 4. For-Each blocks: four pieces with a deformer and colours keyed by
    // the pass; eight counted passes of an animated wave.
    let chunks = n.chain(&mut e, column, "pnt.piece", &[("size", json!(2048))]);
    let begin = n.chain(
        &mut e,
        chunks,
        "pnt.foreach",
        &[("mode", json!("pieces")), ("iterations", json!(4))],
    );
    let tint = n.chain(
        &mut e,
        begin,
        "pnt.randomize",
        &[("key", json!("iteration"))],
    );
    let lean = n.chain(&mut e, tint, "pnt.twist", &[]);
    n.animate(lean, "angle", "45 * sin(time)");
    let end = n.chain(&mut e, lean, "pnt.foreach_end", &[]);
    n.wire(end, begin, "end");
    let begin2 = n.chain(&mut e, sphere, "pnt.foreach", &[("iterations", json!(8))]);
    let ripple = n.chain(&mut e, begin2, "pnt.wave", &[]);
    n.animate(ripple, "phase", "time * 45");
    let end2 = n.chain(&mut e, ripple, "pnt.foreach_end", &[]);
    n.wire(end2, begin2, "end");
    let r5 = n.node(&mut e, "img.render_points", &[]);
    n.wire(end, r5, "geo");
    let r6 = n.node(&mut e, "img.render_points", &[]);
    n.wire(end2, r6, "geo");
    e.edit("chains", n.edits).unwrap();
    let out = [r1, r2, r3, r4, r5, r6];
    let mut t = 0.0;
    for _ in 0..40 {
        e.cook(&out, t).unwrap();
        t += 1.0 / 60.0;
    }
    // Fused pipelines are built on a worker thread: warm up until they are
    // in use (Bend + Wave in chain 2 fuse).
    for _ in 0..5000 {
        if !e.pending() {
            break;
        }
        e.cook(&out, t).unwrap();
        t += 1.0 / 60.0;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!e.pending(), "fused pipelines built");
    e.cook(&out, t).unwrap();
    t += 1.0 / 60.0;
    assert!(e.fused_last_frame() >= 2, "a fused run is part of the test");
    for node in [
        sys, emit, floor, colour, cut, rand, nrm, links, solver, age, begin, end, begin2, end2, r1,
        r2, r3, r4, r5, r6,
    ] {
        assert_eq!(e.error(node), None);
    }
    let (buffers, binds, textures) = (
        e.buffer_pool().created(),
        bind_groups_created_total(),
        e.pool().created(),
    );
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
        "PNT batch steady state: {total} allocations over {frames} frames, {} node-cooks, {per_node:.1} per node-cook",
        cooked
    );
    assert_eq!(e.buffer_pool().created(), buffers, "no buffers created");
    assert_eq!(bind_groups_created_total(), binds, "no bind groups created");
    assert_eq!(e.pool().created(), textures, "no textures created");
    assert!(
        per_node <= BOUND,
        "{per_node:.1} allocations per node-cook (bound {BOUND})"
    );
}
