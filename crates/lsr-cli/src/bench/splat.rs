//! FR-C1 (M6): a synthetic 1M-splat capture (SH degree 3) drawn by Splat
//! MAT through `img.render` at 1920×1080, the camera orbiting so the splats
//! are re-sorted every frame. The 60 fps target is judged on the RTX 4070
//! lab PC; other adapters report what they reach.

use std::time::Instant;

use anyhow::Context as _;
use lsr_gpu::GpuContext;

use super::{Case, Status, timed};

/// Splats in the bench capture.
const SPLATS: usize = 1_000_000;

pub fn splats_1m(gpu: &GpuContext) -> anyhow::Result<Case> {
    use lsr_core::graph::{Edit, Endpoint, Node, Param, Wire};
    use lsr_core::registry::{OpFamily, OperatorSchema, SocketSchema, WireType};
    use lsr_gpu::engine::{Engine, EngineConfig, GeoCtx, GeoOp, OpLibrary};
    use serde_json::json;

    struct Cloud(lsr_gpu::geo::Geometry);
    impl GeoOp for Cloud {
        fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
            *ctx.output = self.0.clone();
            Ok(())
        }
    }
    let cloud = lsr_ops_pnt::splat_file::synthetic(SPLATS, 3).to_geometry(None);
    let features = lsr_gpu::optional_features(&gpu.adapter)
        | (gpu.adapter.features() & wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = pollster::block_on(gpu.adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lsr-bench-splats"),
        required_features: features,
        required_limits: lsr_gpu::required_limits(&gpu.adapter),
        ..Default::default()
    }))
    .context("creating the splat bench device")?;
    let mut registry = crate::builtin_registry();
    registry
        .register(OperatorSchema {
            type_id: "pnt.bench_splats".into(),
            family: OpFamily::Pnt,
            label: "Bench Splats".into(),
            summary: "The splat bench's capture.".into(),
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
    lsr_ops_mat::library(&mut library);
    library.add_geo("pnt.bench_splats", move |_| Box::new(Cloud(cloud.clone())));
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
    let edits = vec![
        Edit::AddNode {
            node: Node::new(src, "pnt.bench_splats", "capture"),
        },
        Edit::AddNode {
            node: Node::new(mat, "mat.splat", "splat1"),
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
        Edit::SetParam {
            node: draw,
            param: "distance".into(),
            value: Some(Param::constant(json!(2.6))),
        },
        // The camera orbits with time: every frame re-sorts.
        Edit::SetParam {
            node: draw,
            param: "azimuth".into(),
            value: Some(Param {
                value: json!(0.0),
                expr: Some("time * 20".into()),
                bind: None,
            }),
        },
    ];
    engine
        .edit("splat bench", edits)
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
        lsr_gpu::geo::kernels::bind_groups_created_total(),
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
        lsr_gpu::geo::kernels::bind_groups_created_total(),
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
    let image = engine.result(draw).context("the render has no image")?;
    let rgba = lsr_interop::readback::read_rgba8(engine.device(), engine.queue(), image)
        .context("reading the image back")?;
    let lit = rgba.chunks_exact(4).filter(|p| p[..3] != [0, 0, 0]).count();
    let cover = 100.0 * lit as f64 / f64::from(w * h);
    anyhow::ensure!(
        cover > 5.0,
        "the splats cover only {cover:.1}% of the image"
    );
    let gpu_split = match lsr_gpu::timer::GpuTimer::new(engine.device(), engine.queue()) {
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
        id: "render.splats_1m",
        status: Status::Ok,
        ms: Some(p50),
        note: Some(format!(
            "{SPLATS} Gaussian splats (SH degree 3, GPU radix sort every frame, 4× MSAA; {cover:.0}% of the image covered) at {w}×{h}, {} frames: p50 {p50:.2} ms ({:.1} fps), p95 {p95:.2} ms, max {max:.2} ms{gpu_split}; render-thread cook avg {cook:.0} µs; first frame (upload) {first_ms:.1} ms; no buffers, bind groups or textures created after warm-up; FR-C1 target 60 fps on the RTX 4070{}",
            frames.len(),
            1000.0 / p50,
            if software { " (software adapter)" } else { "" }
        )),
    })
}
