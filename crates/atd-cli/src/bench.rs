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
