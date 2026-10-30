//! `atidarla bench`: the standard performance suite (PRD §8, FR-H4).
//!
//! M0 measures start-up costs only. Each milestone adds the PT-n cases it is
//! accountable for; results are compared across runs by case `id`.

use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use atd_core::PlatformInfo;
use atd_gpu::{AdapterReport, GpuContext};
use serde::Serialize;

/// Bumped whenever the JSON layout changes incompatibly.
const SCHEMA: &str = "atidarla-bench/1";

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    atidarla: &'static str,
    unix_time: u64,
    platform: PlatformInfo,
    adapter: Option<AdapterReport>,
    cases: Vec<Case>,
}

#[derive(Debug, Serialize)]
struct Case {
    id: &'static str,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Ok,
    Skipped,
    Error,
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed().as_secs_f64() * 1e3)
}

/// Stand-in for PT-1's "kernel" step until the Kernel DSL exists: a 3×3
/// sharpen as an ISF shader.
const SHARPEN: &str = r#"/*{"INPUTS": [{"NAME": "inputImage", "TYPE": "image"}]}*/
void main() {
    vec2 px = 1.0 / RENDERSIZE;
    vec2 uv = isf_FragNormCoord;
    vec4 c = IMG_NORM_PIXEL(inputImage, uv) * 5.0;
    c -= IMG_NORM_PIXEL(inputImage, uv + vec2(px.x, 0.0));
    c -= IMG_NORM_PIXEL(inputImage, uv - vec2(px.x, 0.0));
    c -= IMG_NORM_PIXEL(inputImage, uv + vec2(0.0, px.y));
    c -= IMG_NORM_PIXEL(inputImage, uv - vec2(0.0, px.y));
    gl_FragColor = vec4(c.rgb, 1.0);
}
"#;

/// PT-1 (PRD §8): a 20-node IMG chain (composite, blur, feedback, LUT,
/// kernel and more) at 3840×2160, animated by an LFO so every node cooks
/// every frame. Frame time includes GPU completion. The 60 fps target is
/// judged on the RTX 4070 lab PC; other adapters report what they reach
/// (software adapters run fewer frames).
fn pt1(gpu: &GpuContext) -> anyhow::Result<Case> {
    use atd_core::graph::{Edit, Endpoint, Node, Param, Wire};
    use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
    use serde_json::json;

    let mut registry = crate::builtin_registry();
    let mut library = OpLibrary::default();
    atd_ops_img::library(&mut library);
    atd_ops_sig::library(&mut library);
    let kernel = atd_ops_img::isf::IsfDef::new("bench sharpen", SHARPEN, None)
        .map_err(|e| anyhow::anyhow!("sharpen kernel: {e}"))?;
    let kernel_id = kernel.schema.type_id.clone();
    atd_ops_img::isf::register(vec![kernel], &mut registry, &mut library);
    let (w, h) = (3840, 2160);
    let mut engine = Engine::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        registry,
        library,
        EngineConfig {
            width: w,
            height: h,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );

    // (type, inputs as (socket, index of the source node), parameters)
    type Spec<'a> = (
        &'a str,
        Vec<(&'a str, usize)>,
        Vec<(&'a str, serde_json::Value)>,
    );
    let chain: Vec<Spec> = vec![
        ("img.noise", vec![], vec![("monochrome", json!(false))]),
        ("img.ramp", vec![], vec![("type", json!("radial"))]),
        (
            "img.composite",
            vec![("a", 0), ("b", 1)],
            vec![("operation", json!("multiply"))],
        ),
        ("img.blur", vec![("in", 2)], vec![]),
        ("img.feedback", vec![("in", 3), ("loop", 19)], vec![]),
        ("img.lookup", vec![("in", 4), ("lut", 1)], vec![]),
        ("img.hsv_adjust", vec![("in", 5)], vec![]),
        (
            "img.transform",
            vec![("in", 6)],
            vec![("rotate", json!(3.0))],
        ),
        (kernel_id.as_str(), vec![("inputImage", 7)], vec![]),
        ("img.kaleidoscope", vec![("in", 8)], vec![]),
        ("img.displace", vec![("in", 9), ("by", 0)], vec![]),
        ("img.edge", vec![("in", 10)], vec![]),
        ("img.blend", vec![("a", 11), ("b", 5)], vec![]),
        ("img.bloom", vec![("in", 12)], vec![]),
        ("img.level", vec![("in", 13)], vec![]),
        ("img.mirror", vec![("in", 14)], vec![]),
        ("img.tile", vec![("in", 15)], vec![]),
        ("img.channel_mix", vec![("in", 16)], vec![]),
        ("img.blur", vec![("in", 17)], vec![]),
        ("img.level", vec![("in", 18)], vec![]),
    ];
    let ids: Vec<_> = chain.iter().map(|_| engine.alloc_id()).collect();
    let lfo = engine.alloc_id();
    let mut edits = vec![Edit::AddNode {
        node: Node::new(lfo, "sig.lfo", "lfo1"),
    }];
    for (i, (ty, _, params)) in chain.iter().enumerate() {
        edits.push(Edit::AddNode {
            node: Node::new(ids[i], *ty, format!("n{i}")),
        });
        for (p, v) in params {
            edits.push(Edit::SetParam {
                node: ids[i],
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
    }
    for (i, (_, inputs, _)) in chain.iter().enumerate() {
        for (socket, src) in inputs {
            edits.push(Edit::Connect {
                wire: Wire::new(
                    Endpoint::new(ids[*src], "out"),
                    Endpoint::new(ids[i], *socket),
                ),
            });
        }
    }
    edits.push(Edit::SetParam {
        node: ids[0],
        param: "z".into(),
        value: Some(Param::bound(json!(0.0), lfo, "lfo")),
    });
    engine.edit("pt1", edits).context("building the chain")?;
    let out = ids[ids.len() - 1];

    // Warm up: shaders compile (ISF on a worker thread), pools fill.
    let mut t = 0.0;
    let warm = Instant::now();
    loop {
        engine.cook_frame(Some(out), t)?;
        let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
        t += 1.0 / 60.0;
        if !engine.pending() && warm.elapsed().as_secs_f64() > 0.5 {
            break;
        }
        anyhow::ensure!(warm.elapsed().as_secs() < 120, "warm-up did not finish");
    }
    let software = gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    let budget = if software { 3.0 } else { 5.0 };
    let mut frames = Vec::new();
    let start = Instant::now();
    let min_frames = if software { 3 } else { 10 };
    while frames.len() < 300
        && (frames.len() < min_frames || start.elapsed().as_secs_f64() < budget)
    {
        let (cooked, ms) = timed(|| {
            let n = engine.cook_frame(Some(out), t);
            let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
            n
        });
        anyhow::ensure!(cooked? >= ids.len(), "every node cooks every frame");
        frames.push(ms);
        t += 1.0 / 60.0;
    }
    if let Some(e) = ids.iter().find_map(|id| engine.error(*id)) {
        anyhow::bail!("a node failed: {e}");
    }
    frames.sort_by(f64::total_cmp);
    let pct = |p: f64| frames[((frames.len() - 1) as f64 * p).round() as usize];
    let (p50, p95, max) = (pct(0.5), pct(0.95), frames[frames.len() - 1]);
    Ok(Case {
        id: "pt1.img_chain_4k",
        status: Status::Ok,
        ms: Some(p50),
        note: Some(format!(
            "{} nodes at {w}×{h}, {} frames: p50 {p50:.2} ms ({:.1} fps), p95 {p95:.2} ms, max {max:.2} ms; target p95 ≤ 16.7 ms on RTX 4070{}",
            ids.len(),
            frames.len(),
            1000.0 / p50,
            if software { " (software adapter)" } else { "" }
        )),
    })
}

/// PT-2 (PRD §8, M2 AC1): 2M particles with curl noise, attraction and age,
/// drawn as 2-pixel additive sprites at 1920×1080 (the definition proposed
/// in docs/design/geometry-attributes.md §4, Q1). Frame time includes GPU
/// completion; with timestamp queries the GPU time is split into simulate
/// and draw (simulate alone, then both). The target is judged on the RTX
/// 4070 lab PC.
fn pt2(gpu: &GpuContext) -> anyhow::Result<Case> {
    use atd_core::graph::{Edit, Endpoint, Node, Param, Wire};
    use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
    use serde_json::json;

    // Its own device, with timestamps when the adapter has them.
    let features = atd_gpu::optional_features(&gpu.adapter)
        | (gpu.adapter.features() & wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = pollster::block_on(gpu.adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("atd-bench-pt2"),
        required_features: features,
        required_limits: atd_gpu::required_limits(&gpu.adapter),
        ..Default::default()
    }))
    .context("creating the PT-2 device")?;
    let registry = crate::builtin_registry();
    let mut library = OpLibrary::default();
    atd_ops_pnt::library(&mut library);
    let (w, h) = (1920, 1080);
    let mut engine = Engine::new(
        device,
        queue,
        registry,
        library,
        EngineConfig {
            width: w,
            height: h,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    let (sys, draw) = (engine.alloc_id(), engine.alloc_id());
    let mut edits = vec![
        Edit::AddNode {
            node: Node::new(sys, "pnt.particles", "particles1"),
        },
        Edit::AddNode {
            node: Node::new(draw, "img.render_points", "render1"),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(sys, "out"), Endpoint::new(draw, "geo")),
        },
    ];
    let params = [
        (sys, "capacity", json!(2_000_000)),
        (sys, "initial", json!(2_000_000)),
        (sys, "rate", json!(500_000.0)),
        (sys, "life", json!(4.0)),
        (sys, "attract", json!(0.5)),
        (draw, "intensity", json!(0.3)),
        (draw, "height", json!(3.0)),
    ];
    for (node, param, value) in params {
        edits.push(Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param::constant(value)),
        });
    }
    engine.edit("pt2", edits).context("building the patch")?;

    let software = gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    let mut t = 0.0;
    for _ in 0..if software { 3 } else { 60 } {
        engine.cook(&[draw], t)?;
        let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
        t += 1.0 / 60.0;
    }
    for id in [sys, draw] {
        if let Some(e) = engine.error(id) {
            anyhow::bail!("a node failed: {e}");
        }
    }
    let created = (
        engine.buffer_pool().created(),
        atd_gpu::geo::kernels::bind_groups_created_total(),
        engine.pool().created(),
    );
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * p).round() as usize]
    };
    let frames_wanted = if software { 3 } else { 300 };
    let mut frames = Vec::new();
    for _ in 0..frames_wanted {
        let (cooked, ms) = timed(|| {
            let n = engine.cook(&[draw], t);
            let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
            n
        });
        anyhow::ensure!(cooked? == 2, "both nodes cook every frame");
        frames.push(ms);
        t += 1.0 / 60.0;
    }
    let after = (
        engine.buffer_pool().created(),
        atd_gpu::geo::kernels::bind_groups_created_total(),
        engine.pool().created(),
    );
    anyhow::ensure!(
        after == created,
        "steady frames created GPU objects: {created:?} → {after:?}"
    );
    let (p50, p95, max) = (
        pct(&mut frames, 0.5),
        pct(&mut frames, 0.95),
        frames[frames.len() - 1],
    );
    let split = match atd_gpu::timer::GpuTimer::new(engine.device(), engine.queue()) {
        Some(timer) => {
            let gpu_time = |engine: &mut Engine, node, t: &mut f64| -> anyhow::Result<f64> {
                let mut v = Vec::new();
                for _ in 0..if software { 2 } else { 100 } {
                    timer.start(engine.device(), engine.queue());
                    engine.cook(&[node], *t)?;
                    v.push(
                        timer
                            .stop(engine.device(), engine.queue())
                            .map_err(anyhow::Error::msg)?,
                    );
                    *t += 1.0 / 60.0;
                }
                Ok(pct(&mut v, 0.5))
            };
            let sim = gpu_time(&mut engine, sys, &mut t)?;
            let both = gpu_time(&mut engine, draw, &mut t)?;
            format!(
                "; GPU p50: simulate {sim:.2} ms, simulate + draw {both:.2} ms (draw ≈ {:.2} ms)",
                both - sim
            )
        }
        None => "; no timestamp queries on this adapter, so no GPU split".into(),
    };
    Ok(Case {
        id: "pt2.particles_2m",
        status: Status::Ok,
        ms: Some(p50),
        note: Some(format!(
            "2M particles (curl noise, attract, age), 2-px additive sprites at {w}×{h}, {} frames: p50 {p50:.2} ms ({:.1} fps), p95 {p95:.2} ms, max {max:.2} ms{split}; no buffers, bind groups or textures created after warm-up; target p95 ≤ 16.7 ms on RTX 4070{}",
            frames.len(),
            1000.0 / p50,
            if software { " (software adapter)" } else { "" }
        )),
    })
}

/// A latitude-longitude sphere of `n × n` quads with smooth normals (the
/// geometry of `render.mesh_1m`; MSH primitives are not in this crate).
fn bench_sphere(n: u32) -> atd_gpu::geo::Geometry {
    use atd_gpu::geo::{Attribute, Geometry, PrimKind, Topology};
    let side = n + 1;
    let mut p = Vec::with_capacity((side * side) as usize);
    for j in 0..side {
        let lat = std::f32::consts::PI * (j as f32 / n as f32 - 0.5);
        for i in 0..side {
            let lon = std::f32::consts::TAU * i as f32 / n as f32;
            p.push([lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]);
        }
    }
    let mut corners = Vec::with_capacity((n * n * 4) as usize);
    for j in 0..n {
        for i in 0..n {
            let a = j * side + i;
            // Counter-clockwise seen from outside.
            corners.extend_from_slice(&[a, a + 1, a + side + 1, a + side]);
        }
    }
    let quads: Vec<(PrimKind, &[u32])> = corners
        .chunks_exact(4)
        .map(|c| (PrimKind::Polygon, c))
        .collect();
    let mut g = Geometry::new();
    g.set_points(side * side, None);
    g.set_topology(Some(Topology::from_prims(&quads)));
    g.set_point(Attribute::from_vec3("N", &p));
    g.set_point(Attribute::from_vec3("P", &p));
    g
}

/// Render IMG at M2 scale: a 1M-triangle mesh (a sphere of 708 × 708 quads,
/// smooth normals) drawn by `img.render` with a PBR material, 4× MSAA,
/// depth and back-face culling at 1920×1080, the camera orbiting so the
/// render cooks every frame. Frame time includes GPU completion; the GPU
/// time of the render alone comes from timestamp queries. The draw records
/// are built once (the topology does not change).
fn render_mesh(gpu: &GpuContext) -> anyhow::Result<Case> {
    use atd_core::graph::{Edit, Endpoint, Node, Param, Wire};
    use atd_core::registry::{OpFamily, OperatorSchema, SocketSchema, WireType};
    use atd_gpu::engine::{Engine, EngineConfig, GeoCtx, GeoOp, OpLibrary};
    use serde_json::json;

    struct Mesh(atd_gpu::geo::Geometry);
    impl GeoOp for Mesh {
        fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
            *ctx.output = self.0.clone();
            Ok(())
        }
    }
    let quads = 708;
    let mesh = bench_sphere(quads);
    let triangles = 2 * u64::from(quads) * u64::from(quads);
    let features = atd_gpu::optional_features(&gpu.adapter)
        | (gpu.adapter.features() & wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = pollster::block_on(gpu.adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("atd-bench-render"),
        required_features: features,
        required_limits: atd_gpu::required_limits(&gpu.adapter),
        ..Default::default()
    }))
    .context("creating the render bench device")?;
    let mut registry = crate::builtin_registry();
    registry
        .register(OperatorSchema {
            type_id: "msh.bench_sphere".into(),
            family: OpFamily::Msh,
            label: "Bench Sphere".into(),
            summary: "The render bench's mesh.".into(),
            version: 1,
            params: vec![],
            inputs: vec![],
            outputs: vec![SocketSchema {
                name: "out".into(),
                wire: WireType::Geo,
                optional: true,
                feedback: false,
            }],
            capabilities: vec![],
            cook_budget_us: 1,
            time_dependent: false,
            sink: false,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut library = OpLibrary::default();
    atd_ops_mat::library(&mut library);
    let shared = mesh.clone();
    library.add_geo("msh.bench_sphere", move |_| Box::new(Mesh(shared.clone())));
    let (w, h) = (1920, 1080);
    let mut engine = Engine::new(
        device,
        queue,
        registry,
        library,
        EngineConfig {
            width: w,
            height: h,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    let (src, mat, draw) = (engine.alloc_id(), engine.alloc_id(), engine.alloc_id());
    let mut edits = vec![
        Edit::AddNode {
            node: Node::new(src, "msh.bench_sphere", "sphere1"),
        },
        Edit::AddNode {
            node: Node::new(mat, "mat.pbr", "pbr1"),
        },
        Edit::AddNode {
            node: Node::new(draw, "img.render", "render1"),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(src, "out"), Endpoint::new(draw, "geo")),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(mat, "out"), Endpoint::new(draw, "mat")),
        },
    ];
    let params = [
        (draw, "distance", json!(2.6)),
        (mat, "roughness", json!(0.35)),
    ];
    for (node, param, value) in params {
        edits.push(Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param::constant(value)),
        });
    }
    // The camera orbits with time, so the render cooks every frame.
    edits.push(Edit::SetParam {
        node: draw,
        param: "azimuth".into(),
        value: Some(Param {
            value: json!(0.0),
            expr: Some("time * 20".into()),
            bind: None,
        }),
    });
    engine
        .edit("render bench", edits)
        .context("building the patch")?;

    let software = gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    let mut t = 0.0;
    let warm = Instant::now();
    engine.cook(&[draw], t)?;
    let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
    let first_ms = warm.elapsed().as_secs_f64() * 1e3;
    for _ in 0..if software { 2 } else { 30 } {
        t += 1.0 / 60.0;
        engine.cook(&[draw], t)?;
        let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
    }
    if let Some(e) = [src, mat, draw].iter().find_map(|id| engine.error(*id)) {
        anyhow::bail!("a node failed: {e}");
    }
    let created = (
        engine.buffer_pool().created(),
        atd_gpu::geo::kernels::bind_groups_created_total(),
        engine.pool().created(),
    );
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * p).round() as usize]
    };
    let mut frames = Vec::new();
    for _ in 0..if software { 3 } else { 300 } {
        t += 1.0 / 60.0;
        let (cooked, ms) = timed(|| {
            let n = engine.cook(&[draw], t);
            let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
            n
        });
        anyhow::ensure!(cooked? == 1, "the render cooks every frame");
        frames.push(ms);
    }
    let after = (
        engine.buffer_pool().created(),
        atd_gpu::geo::kernels::bind_groups_created_total(),
        engine.pool().created(),
    );
    anyhow::ensure!(
        after == created,
        "steady frames created GPU objects: {created:?} → {after:?}"
    );
    let (p50, p95, max) = (
        pct(&mut frames, 0.5),
        pct(&mut frames, 0.95),
        frames[frames.len() - 1],
    );
    let cook = engine.stat(draw).map_or(0.0, |s| s.avg_us);
    // The sphere must really be on screen: count pixels off the background.
    let image = engine.result(draw).context("the render has no image")?;
    let rgba = atd_interop::readback::read_rgba8(engine.device(), engine.queue(), image)
        .context("reading the image back")?;
    let lit = rgba.chunks_exact(4).filter(|p| p[..3] != [0, 0, 0]).count();
    let cover = 100.0 * lit as f64 / f64::from(w * h);
    anyhow::ensure!(cover > 5.0, "the mesh covers only {cover:.1}% of the image");
    let gpu_split = match atd_gpu::timer::GpuTimer::new(engine.device(), engine.queue()) {
        Some(timer) => {
            let mut v = Vec::new();
            for _ in 0..if software { 2 } else { 100 } {
                t += 1.0 / 60.0;
                timer.start(engine.device(), engine.queue());
                engine.cook(&[draw], t)?;
                v.push(
                    timer
                        .stop(engine.device(), engine.queue())
                        .map_err(anyhow::Error::msg)?,
                );
            }
            format!("; GPU p50 {:.2} ms", pct(&mut v, 0.5))
        }
        None => "; no timestamp queries on this adapter".into(),
    };
    Ok(Case {
        id: "render.mesh_1m",
        status: Status::Ok,
        ms: Some(p50),
        note: Some(format!(
            "{triangles} triangles (PBR, 4× MSAA, depth, culling; {cover:.0}% of the image covered) at {w}×{h}, {} frames: p50 {p50:.2} ms ({:.1} fps), p95 {p95:.2} ms, max {max:.2} ms{gpu_split}; render-thread cook avg {cook:.0} µs; first frame (upload and triangulation) {first_ms:.1} ms; no buffers, bind groups or textures created after warm-up{}",
            frames.len(),
            1000.0 / p50,
            if software { " (software adapter)" } else { "" }
        )),
    })
}

/// PT-7 groundwork (PRD §8, M3 AC3): frames from a clip launch (an edit
/// of a Layer's `target`) to the new clip in the layer's output, with a cut
/// and no quantise, at 1920×1080. Eight clips cook every frame (an LFO
/// animates the noise ones); the measured slots alternate between two
/// constants so the switch shows in one pixel. The PT-7 targets (≤ 1 frame
/// for HAP, ≤ 3 for hardware H.264/H.265) need Movie In clips and the RTX
/// 4070 lab PC; this case measures the layer itself.
fn pt7_launch_cut(gpu: &GpuContext) -> anyhow::Result<Case> {
    use atd_core::graph::{Edit, Endpoint, Node, Param, Wire};
    use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
    use serde_json::json;

    let mut library = OpLibrary::default();
    atd_ops_img::library(&mut library);
    atd_ops_sig::library(&mut library);
    let (w, h) = (1920, 1080);
    let mut engine = Engine::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        crate::builtin_registry(),
        library,
        EngineConfig {
            width: w,
            height: h,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    let (layer, lfo) = (engine.alloc_id(), engine.alloc_id());
    let mut edits = vec![
        Edit::AddNode {
            node: Node::new(layer, "img.layer", "layer1"),
        },
        Edit::AddNode {
            node: Node::new(lfo, "sig.lfo", "lfo1"),
        },
    ];
    let colours = [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    for slot in 0..8usize {
        let id = engine.alloc_id();
        let (ty, param, value) = match colours.get(slot) {
            Some(c) => ("img.constant", "colour", Param::constant(json!(c))),
            None => ("img.noise", "z", Param::bound(json!(0.0), lfo, "lfo")),
        };
        edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("clip{slot}")),
        });
        edits.push(Edit::SetParam {
            node: id,
            param: param.into(),
            value: Some(value),
        });
        edits.push(Edit::Connect {
            wire: Wire::new(
                Endpoint::new(id, "out"),
                Endpoint::new(layer, format!("clip{slot}")),
            ),
        });
    }
    engine.edit("pt7", edits).context("building the layer")?;
    let target = |slot: usize| Edit::SetParam {
        node: layer,
        param: "target".into(),
        value: Some(Param::constant(json!(slot))),
    };
    // The centre pixel's red channel tells the two measured clips apart.
    let centre_red = |engine: &Engine| -> anyhow::Result<u8> {
        let tex = engine.result(layer).context("the layer has no result")?;
        let px = atd_interop::readback::read_rgba8(engine.device(), engine.queue(), tex)?;
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        Ok(px[i])
    };
    let mut t = 0.0;
    for _ in 0..30 {
        engine.cook_frame(Some(layer), t)?;
        t += 1.0 / 60.0;
    }
    let launches = 20;
    let (mut worst, mut ms) = (0usize, Vec::new());
    for i in 0..launches {
        let slot = i % 2;
        let start = Instant::now();
        engine.edit("launch", [target(slot)])?;
        let mut frames = 0;
        loop {
            engine.cook_frame(Some(layer), t)?;
            let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
            t += 1.0 / 60.0;
            frames += 1;
            if frames == 1 {
                ms.push(start.elapsed().as_secs_f64() * 1e3);
            }
            if (centre_red(&engine)? > 128) == (slot == 0) {
                break;
            }
            anyhow::ensure!(frames < 30, "the new clip never appeared");
        }
        worst = worst.max(frames);
        // A few frames on the clip before the next launch.
        for _ in 0..3 {
            engine.cook_frame(Some(layer), t)?;
            t += 1.0 / 60.0;
        }
    }
    if let Some(e) = engine.error(layer) {
        anyhow::bail!("the layer failed: {e}");
    }
    ms.sort_by(f64::total_cmp);
    let p50 = ms[ms.len() / 2];
    Ok(Case {
        id: "pt7.launch_cut",
        status: Status::Ok,
        ms: Some(p50),
        note: Some(format!(
            "{launches} launches (cut, quantise off) on a layer with 8 clips at {w}×{h}: the new clip is in the output {worst} frame(s) after the edit at worst (1 = the next cook); launch frame p50 {p50:.2} ms including GPU completion. PT-7 targets (HAP ≤ 1, H.264/H.265 ≤ 3 frames) need Movie In clips on the RTX 4070"
        )),
    })
}

pub fn run(out: Option<&Path>, use_gpu: bool) -> anyhow::Result<()> {
    let mut platform = PlatformInfo::host();
    let mut adapter = None;
    let mut cases = Vec::new();

    let (registry, ms) = timed(crate::builtin_registry);
    cases.push(Case {
        id: "startup.registry",
        status: Status::Ok,
        ms: Some(ms),
        note: Some(format!("{} operators registered", registry.len())),
    });

    if use_gpu {
        let (gpu, ms) = timed(GpuContext::headless);
        match gpu {
            Ok(gpu) => {
                platform = gpu.platform_info();
                adapter = Some(gpu.report());
                cases.push(Case {
                    id: "startup.gpu_device",
                    status: Status::Ok,
                    ms: Some(ms),
                    note: None,
                });
                cases.push(match pt1(&gpu) {
                    Ok(case) => case,
                    Err(err) => Case {
                        id: "pt1.img_chain_4k",
                        status: Status::Error,
                        ms: None,
                        note: Some(format!("{err:#}")),
                    },
                });
                cases.push(match pt2(&gpu) {
                    Ok(case) => case,
                    Err(err) => Case {
                        id: "pt2.particles_2m",
                        status: Status::Error,
                        ms: None,
                        note: Some(format!("{err:#}")),
                    },
                });
                cases.push(pt7_launch_cut(&gpu).unwrap_or_else(|err| Case {
                    id: "pt7.launch_cut",
                    status: Status::Error,
                    ms: None,
                    note: Some(format!("{err:#}")),
                }));
                cases.push(render_mesh(&gpu).unwrap_or_else(|err| Case {
                    id: "render.mesh_1m",
                    status: Status::Error,
                    ms: None,
                    note: Some(format!("{err:#}")),
                }));
            }
            Err(err) => cases.push(Case {
                id: "startup.gpu_device",
                status: Status::Error,
                ms: None,
                note: Some(err.to_string()),
            }),
        }
    } else {
        cases.push(Case {
            id: "startup.gpu_device",
            status: Status::Skipped,
            ms: None,
            note: Some("--no-gpu".into()),
        });
    }

    let report = Report {
        schema: SCHEMA,
        atidarla: atd_core::VERSION,
        unix_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        platform,
        adapter,
        cases,
    };
    let json = serde_json::to_string_pretty(&report)?;
    match out {
        Some(path) => std::fs::write(path, json + "\n")
            .with_context(|| format!("writing {}", path.display()))?,
        None => println!("{json}"),
    }
    Ok(())
}
