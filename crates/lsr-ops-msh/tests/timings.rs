//! Worker-side cook times of typical MSH jobs (not a pass/fail test):
//! `cargo test -p lsr-ops-msh --release --test timings -- --ignored --nocapture`.

use std::time::Instant;

use lsr_ops_msh::{Mesh, run};
use serde_json::{Value, json};

fn time(label: &str, ty: &str, params: &[(&str, Value)], inputs: &[Option<Mesh>]) -> Mesh {
    let mut best = f64::MAX;
    let mut out = Mesh::new();
    for _ in 0..3 {
        let t = Instant::now();
        out = run(ty, params, inputs).unwrap();
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!(
        "{label:<46} {best:>9.2} ms  ({} points, {} prims)",
        out.points(),
        out.prims()
    );
    out
}

#[test]
#[ignore = "timings: run with --release --ignored --nocapture"]
fn worker_cook_times() {
    let sphere = time(
        "sphere polar 256 x 512",
        "msh.sphere",
        &[("rows", json!(256)), ("columns", json!(512))],
        &[],
    );
    let boxed = run("msh.box", &[], &[]).unwrap();
    time(
        "subdivide box, 5 levels",
        "msh.subdivide",
        &[("levels", json!(5))],
        &[Some(boxed)],
    );
    time(
        "normals, cusp 30, of the sphere",
        "msh.normals",
        &[("cusp", json!(30.0))],
        &[Some(sphere.clone())],
    );
    time(
        "polyreduce sphere to 10%",
        "msh.polyreduce",
        &[("percent", json!(10.0))],
        &[Some(sphere.clone())],
    );
    let ico = run(
        "msh.sphere",
        &[("type", json!("ico")), ("level", json!(4))],
        &[],
    )
    .unwrap();
    let cutter = run("msh.box", &[("centre", json!([0.3, 0.3, 0.3]))], &[]).unwrap();
    time(
        "boolean subtract, ico level 4 (5120 tris) - box",
        "msh.boolean",
        &[("operation", json!("subtract"))],
        &[Some(ico), Some(cutter)],
    );
    let pts = time(
        "scatter 1M points on the sphere",
        "msh.scatter",
        &[("count", json!(1_000_000))],
        &[Some(sphere.clone())],
    );
    let few = run(
        "msh.scatter",
        &[("count", json!(2000))],
        &[Some(sphere.clone())],
    )
    .unwrap();
    time(
        "iso surface, 2000 points, resolution 128",
        "msh.iso_surface",
        &[("radius", json!(0.05)), ("resolution", json!(128))],
        &[Some(few)],
    );
    time(
        "extrude every polygon of the sphere",
        "msh.extrude",
        &[],
        &[Some(sphere.clone())],
    );
    let divided = run("msh.box", &[("divisions", json!(64))], &[]).unwrap();
    time(
        "bevel box of 64 divisions, 3 segments",
        "msh.bevel",
        &[("offset", json!(0.01)), ("segments", json!(3))],
        &[Some(divided.clone())],
    );
    let open = run(
        "msh.clip",
        &[("origin", json!([0.0, 0.4, 0.0]))],
        &[Some(divided)],
    )
    .unwrap();
    time(
        "bevel open box (clipped), 3 segments",
        "msh.bevel",
        &[("offset", json!(0.01)), ("segments", json!(3))],
        &[Some(open)],
    );
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let glb = dir.join("timing.glb");
    time(
        "file out .glb (131k points)",
        "msh.file_out",
        &[("file", json!(glb.to_string_lossy()))],
        &[Some(sphere.clone())],
    );
    time(
        "file in .glb",
        "msh.file_in",
        &[("file", json!(glb.to_string_lossy()))],
        &[],
    );
    let t = Instant::now();
    let g = pts.to_geometry(None);
    eprintln!(
        "{:<46} {:>9.2} ms",
        "Mesh -> GEO conversion, 1M points",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let back = Mesh::from_geometry(&g, None).unwrap();
    eprintln!(
        "{:<46} {:>9.2} ms",
        "GEO -> Mesh conversion, 1M points",
        t.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(back.points(), pts.points());
}

/// The render-thread side of an MSH node that recooks every frame (a
/// time expression), live at 60 fps over a 130k-point sphere: snapshot,
/// comparison, hand-over and swap.
#[test]
#[ignore = "timings: run with --release --ignored --nocapture"]
fn render_thread_cost_live() {
    use lsr_core::graph::{Edit, Node, Param};
    use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: lsr_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .unwrap();
    let mut reg = lsr_core::registry::Registry::default();
    lsr_ops_msh::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_msh::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 64,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let (s, t) = (e.alloc_id(), e.alloc_id());
    let param = |node, name: &str, value: Value, expr: Option<&str>| Edit::SetParam {
        node,
        param: name.into(),
        value: Some(Param {
            value,
            expr: expr.map(Into::into),
            bind: None,
        }),
    };
    e.edit(
        "build",
        vec![
            Edit::AddNode {
                node: Node::new(s, "msh.sphere", "s"),
            },
            Edit::AddNode {
                node: Node::new(t, "msh.transform", "t"),
            },
            param(s, "rows", json!(256), None),
            param(s, "columns", json!(512), None),
            param(t, "uniform", json!(1.0), Some("1 + 0.1 * sin(time)")),
            Edit::Connect {
                wire: lsr_core::graph::Wire::new(
                    lsr_core::graph::Endpoint::new(s, "out"),
                    lsr_core::graph::Endpoint::new(t, "in"),
                ),
            },
        ],
    )
    .unwrap();
    let frame = std::time::Duration::from_secs_f64(1.0 / 60.0);
    let start = Instant::now();
    let mut cooks = Vec::new();
    for i in 0..300u32 {
        let t0 = Instant::now();
        e.cook(&[t], f64::from(i) / 60.0).unwrap();
        cooks.push(t0.elapsed().as_secs_f64() * 1e3);
        if let Some(w) = (frame * (i + 1)).checked_sub(start.elapsed()) {
            std::thread::sleep(w);
        }
    }
    let mut tail = cooks[30..].to_vec();
    tail.sort_by(f64::total_cmp);
    let pct = |p: f64| tail[((tail.len() - 1) as f64 * p) as usize];
    let st = e.stat(t).unwrap();
    eprintln!(
        "render thread, whole frame (sphere + transform of 130k points every frame): p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms; transform node avg {:.0} us, max {:.0} us; status {:?}",
        pct(0.5),
        pct(0.95),
        pct(1.0),
        st.avg_us,
        st.max_us,
        e.status(t)
    );
}
