//! Rule 2 for GEO cooks: after warm-up, frames create no buffers or bind
//! groups and the render thread allocates little. wgpu itself allocates
//! while recording passes, so (as for IMG, `atd-ops-img/tests/alloc.rs`) the
//! test bounds allocations per node-cook rather than demanding zero.
//!
//! One test in this binary: the bind-group counter is process-wide.

use std::alloc::{GlobalAlloc, Layout, System};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::kernels::bind_groups_created_total;
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

/// Allocations per node-cook allowed. Measured on the RTX 5070 Ti (DX12,
/// 2026-10-18; run with `ATD_ALLOC_BREAKDOWN=1` for the split): 34.9 per
/// node-cook, nearly all wgpu's: about 16-24 allocations per compute or
/// render pass at submit and 4 per `write_buffer` (uniforms change every
/// frame here), against 17 for an empty frame. The GEO code itself adds
/// about 10 per frame over the six nodes (an `Arc` per new attribute
/// generation and a `Vec` per copy-on-write clone). IMG allows 25 with one
/// render pass per node. The Metal runner in CI measured 72.7: wgpu records
/// more per pass there, so the bound leaves room for it and still sits far
/// below what per-frame bind groups or buffers would add.
const BOUND: f64 = 90.0;

#[test]
fn geo_chains_steady_state_allocations() {
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
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let mut edits = Vec::new();
    let mut node = |e: &mut Engine, ty: &str, params: &[(&str, serde_json::Value)]| {
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
    };
    // A static source deformed by two animated noises (copy on write, then
    // in place), drawn; and a particle system with a force in its loop.
    let pts = node(&mut e, "pnt.points", &[("count", json!(4096))]);
    let n1 = node(&mut e, "pnt.noise", &[]);
    let n2 = node(&mut e, "pnt.noise", &[]);
    let r1 = node(&mut e, "img.render_points", &[]);
    let ps = node(
        &mut e,
        "pnt.particles",
        &[("capacity", json!(10_000)), ("rate", json!(20_000.0))],
    );
    let force = node(&mut e, "pnt.noise", &[("mode", json!("velocity"))]);
    let r2 = node(&mut e, "img.render_points", &[]);
    let wire = |from: NodeId, to: NodeId, socket: &str| Edit::Connect {
        wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
    };
    edits.extend([
        wire(pts, n1, "in"),
        wire(n1, n2, "in"),
        wire(n2, r1, "geo"),
        wire(ps, force, "in"),
        wire(force, ps, "prev"),
        wire(force, r2, "geo"),
    ]);
    for n in [n1, n2] {
        edits.push(Edit::SetParam {
            node: n,
            param: "amplitude".into(),
            value: Some(Param {
                value: json!(0.0),
                expr: Some("0.1 + 0.05 * sin(time)".into()),
                bind: None,
            }),
        });
    }
    e.edit("chain", edits).unwrap();
    let out = [r1, r2];
    let mut t = 0.0;
    for _ in 0..30 {
        e.cook(&out, t).unwrap();
        t += 1.0 / 60.0;
    }
    for n in [pts, n1, n2, r1, ps, force, r2] {
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
        assert_eq!(cooked, 6, "everything but the static source cooks");
        t += 1.0 / 60.0;
    }
    let total = allocs() - before;
    COUNTING.with(|c| c.set(false));
    let per_node = total as f64 / (frames * 6) as f64;
    eprintln!(
        "GEO steady state: {total} allocations over {frames} frames, {per_node:.1} per node-cook"
    );
    assert_eq!(e.buffer_pool().created(), buffers, "no buffers created");
    assert_eq!(bind_groups_created_total(), binds, "no bind groups created");
    assert_eq!(e.pool().created(), textures, "no textures created");
    if std::env::var_os("ATD_ALLOC_BREAKDOWN").is_some() {
        for (label, req) in [
            ("empty", &[][..]),
            ("points chain", &[r1][..]),
            ("particles", &[r2][..]),
        ] {
            for _ in 0..10 {
                e.cook(req, t).unwrap();
                t += 1.0 / 60.0;
            }
            COUNTING.with(|c| c.set(true));
            let before = allocs();
            for _ in 0..frames {
                e.cook(req, t).unwrap();
                t += 1.0 / 60.0;
            }
            let n = allocs() - before;
            COUNTING.with(|c| c.set(false));
            eprintln!(
                "{label}: {:.1} allocations per frame",
                n as f64 / frames as f64
            );
        }
        // wgpu's own costs, for comparison.
        let device = e.device().clone();
        let queue = e.queue().clone();
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 1024,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let kernel = atd_gpu::geo::kernels::Kernel::new(
            &device,
            "probe",
            "@group(0) @binding(0) var<storage, read_write> a: array<u32>;
             @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) g: vec3u) { a[g.x] = g.x; }",
            "main",
            &[atd_gpu::geo::kernels::Bind::ReadWrite],
        );
        let count = |f: &mut dyn FnMut()| {
            f();
            COUNTING.with(|c| c.set(true));
            let before = allocs();
            for _ in 0..100 {
                f();
            }
            let n = allocs() - before;
            COUNTING.with(|c| c.set(false));
            n as f64 / 100.0
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        let one = count(&mut || {
            kernel.dispatch(
                &device,
                &mut encoder,
                &[&buf],
                atd_gpu::geo::kernels::Work::Groups(1),
            )
        });
        let fifteen = count(&mut || {
            let mut pass = atd_gpu::geo::kernels::pass(&mut encoder);
            for _ in 0..15 {
                kernel.dispatch_in(
                    &device,
                    &mut pass,
                    &[&buf],
                    atd_gpu::geo::kernels::Work::Groups(1),
                );
            }
        });
        let write = count(&mut || queue.write_buffer(&buf, 0, &[1u8; 64]));
        queue.submit([encoder.finish()]);
        let (device, queue, kernel, buf) = (&device, &queue, &kernel, &buf);
        let submit = |passes: usize, dispatches: usize| {
            move || {
                let mut encoder = device.create_command_encoder(&Default::default());
                for _ in 0..passes {
                    let mut pass = atd_gpu::geo::kernels::pass(&mut encoder);
                    for _ in 0..dispatches {
                        kernel.dispatch_in(
                            device,
                            &mut pass,
                            &[buf],
                            atd_gpu::geo::kernels::Work::Groups(1),
                        );
                    }
                }
                queue.submit([encoder.finish()]);
            }
        };
        let empty = count(&mut submit(0, 0));
        let p1 = count(&mut submit(1, 1));
        let p15 = count(&mut submit(1, 15));
        let p5 = count(&mut submit(5, 1));
        eprintln!(
            "wgpu: recording a 1-dispatch pass {one:.1}, 15 dispatches in one pass {fifteen:.1}, write_buffer {write:.1}; \
             encoder + submit: empty {empty:.1}, 1 pass × 1 dispatch {p1:.1}, 1 pass × 15 {p15:.1}, 5 passes × 1 {p5:.1}"
        );
    }
    assert!(
        per_node <= BOUND,
        "{per_node:.1} allocations per node-cook (bound {BOUND})"
    );
}
