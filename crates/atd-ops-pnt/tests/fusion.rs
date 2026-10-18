//! S3 follow-up (`docs/milestones/m2/pnt-batch.md`): what fusing a stack of
//! cheap deformers would save. An ignored benchmark that prints its numbers:
//!
//! ```sh
//! cargo test -p atd-ops-pnt --release --test fusion -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Over 2M points: (a) 30 Transform deformers in a row (each turns 1° about
//! Z: one copy on write, then in place), (b) the same stack composed into
//! one Transform (30°), which is what fusing it would dispatch, and (c) the
//! geometry core's 30 curl-noise deformers for reference. (a) and (b) give
//! the same points (checked).

use std::time::Instant;

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::read_blocking;
use atd_gpu::timer::GpuTimer;
use serde_json::{Value, json};

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    let info = adapter.get_info();
    eprintln!("adapter: {} ({:?})", info.name, info.backend);
    let features = atd_gpu::optional_features(&adapter)
        | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()?;
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 64,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, edits: &mut Vec<Edit>, ty: &str, params: &[(&str, Value)]) -> NodeId {
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

/// A stack of `n` operators of type `ty` over 2M random points, animated
/// through `param` (so every node cooks every frame); returns its end.
fn stack(
    e: &mut Engine,
    n: usize,
    ty: &str,
    params: &[(&str, Value)],
    animate: (&str, &str),
) -> NodeId {
    let mut edits = Vec::new();
    let mut last = add(
        e,
        &mut edits,
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(2_000_000))],
    );
    for _ in 0..n {
        let d = add(e, &mut edits, ty, params);
        edits.push(Edit::SetParam {
            node: d,
            param: animate.0.into(),
            value: Some(Param {
                value: json!(0.0),
                expr: Some(animate.1.into()),
                bind: None,
            }),
        });
        edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(last, "out"), Endpoint::new(d, "in")),
        });
        last = d;
    }
    e.edit("stack", edits).unwrap();
    last
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// GPU p50 and p95 over 100 frames after 10 of warm-up, in ms.
fn measure(e: &mut Engine, node: NodeId) -> (f64, f64) {
    let mut t = 0.0;
    for _ in 0..10 {
        e.cook(&[node], t).unwrap();
        let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
        t += 1.0 / 60.0;
    }
    let timer = GpuTimer::new(e.device(), e.queue());
    let mut gpu = Vec::new();
    let start = Instant::now();
    for _ in 0..100 {
        if let Some(timer) = &timer {
            timer.start(e.device(), e.queue());
        }
        e.cook(&[node], t).unwrap();
        match &timer {
            Some(timer) => gpu.push(timer.stop(e.device(), e.queue()).unwrap()),
            None => {
                let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
                gpu.push(f64::NAN);
            }
        }
        t += 1.0 / 60.0;
    }
    eprintln!(
        "  (100 frames in {:.2} s wall)",
        start.elapsed().as_secs_f64()
    );
    (pct(&mut gpu, 0.5), pct(&mut gpu, 0.95))
}

fn p_of(e: &Engine, node: NodeId) -> Vec<f32> {
    let g = e.geometry(node).unwrap();
    let a = g.point("P").unwrap();
    read_blocking(e.device(), e.queue(), e.buffer_pool(), a)
        .unwrap()
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn s3_deformer_stack_fused_and_unfused() {
    // (a) 30 Transforms of 1° each about Z, animated through the uniform
    // scale (1 + 0 × time keeps it at 1 but makes every node cook).
    let Some(mut a) = engine() else { return };
    let rot = [("rotate", json!([0.0, 0.0, 1.0]))];
    let end_a = stack(
        &mut a,
        30,
        "pnt.transform",
        &rot,
        ("uniform_scale", "1 + 0 * time"),
    );
    let (a50, a95) = measure(&mut a, end_a);
    eprintln!(
        "S3 (a) 30 Transform deformers over 2M points, unfused: GPU p50 {a50:.3} ms, p95 {a95:.3} ms"
    );
    // (b) The same stack composed: one Transform of 30°.
    let Some(mut b) = engine() else { return };
    let rot30 = [("rotate", json!([0.0, 0.0, 30.0]))];
    let end_b = stack(
        &mut b,
        1,
        "pnt.transform",
        &rot30,
        ("uniform_scale", "1 + 0 * time"),
    );
    let (b50, b95) = measure(&mut b, end_b);
    eprintln!(
        "S3 (b) the stack composed into one Transform (what fusion dispatches): GPU p50 {b50:.3} ms, p95 {b95:.3} ms"
    );
    let (pa, pb) = (p_of(&a, end_a), p_of(&b, end_b));
    let worst = pa
        .iter()
        .zip(&pb)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    eprintln!("    largest difference between (a) and (b): {worst:.2e}");
    assert!(worst < 1e-4, "{worst}");
    // (c) The geometry core's S3 stack for reference.
    let Some(mut c) = engine() else { return };
    let end_c = stack(
        &mut c,
        30,
        "pnt.noise",
        &[("amplitude", json!(0.01))],
        ("evolution", "time * 0.1"),
    );
    let (c50, c95) = measure(&mut c, end_c);
    eprintln!(
        "S3 (c) 30 curl-noise deformers over 2M points, unfused: GPU p50 {c50:.3} ms, p95 {c95:.3} ms"
    );
}
