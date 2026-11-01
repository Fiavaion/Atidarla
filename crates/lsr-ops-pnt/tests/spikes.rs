//! The geometry spikes of `docs/design/geometry-attributes.md` §4.5, as
//! ignored tests that print their numbers (record them in
//! `docs/milestones/m2/geometry-core.md`). Run one at a time, optimised:
//!
//! ```sh
//! cargo test -p lsr-ops-pnt --release --test spikes -- --ignored --nocapture --test-threads=1
//! ```
//!
//! - **S1**: a static 2M-point grid drawn as 2-px additive sprites at
//!   1920×1080: GPU time, render-thread cook time, and no buffer, bind-group
//!   or texture creation after frame 10.
//! - **S2**: a 2M-particle system (curl noise, attraction, age) drawn at
//!   1080p: simulation and draw GPU time; two offline runs of 300 frames
//!   (the second with every core busy) are byte-identical.
//! - **S3**: 30 noise deformers in a row over 2M points (the hand-over makes
//!   29 of them write in place): GPU time unfused; and a 100k-point round
//!   trip GPU → CPU worker → GPU, live, the worker uploading with
//!   `Queue::write_buffer`: frames of latency and render-thread stalls.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use lsr_core::geo::AttrType;
use lsr_core::registry::{OpFamily, OperatorSchema, SocketSchema, WireType};
use lsr_gpu::engine::{GeoCtx, GeoOp};
use lsr_gpu::geo::readback::{Mapped, Readback};
use lsr_gpu::geo::{Attribute, Geometry};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use lsr_gpu::geo::kernels::bind_groups_created_total;
use lsr_gpu::geo::readback::read_blocking;
use lsr_gpu::timer::GpuTimer;
use serde_json::{Value, json};

fn engine(width: u32, height: u32) -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    let info = adapter.get_info();
    eprintln!("adapter: {} ({:?})", info.name, info.backend);
    let features = lsr_gpu::optional_features(&adapter)
        | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: lsr_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()?;
    let mut reg = Registry::default();
    lsr_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width,
        height,
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

fn wire(edits: &mut Vec<Edit>, from: NodeId, to: NodeId, socket: &str) {
    edits.push(Edit::Connect {
        wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
    });
}

fn expr(edits: &mut Vec<Edit>, node: NodeId, param: &str, e: &str) {
    edits.push(Edit::SetParam {
        node,
        param: param.into(),
        value: Some(Param {
            value: json!(0.0),
            expr: Some(e.into()),
            bind: None,
        }),
    });
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// GPU and CPU time of cooking `nodes` for `frames` frames: (GPU p50, GPU
/// p95, CPU cook p50, CPU cook p95) in ms. GPU times need timestamps.
fn measure(e: &mut Engine, nodes: &[NodeId], frames: usize, t: &mut f64) -> [f64; 4] {
    let timer = GpuTimer::new(e.device(), e.queue());
    let (mut gpu, mut cpu) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        if let Some(timer) = &timer {
            timer.start(e.device(), e.queue());
        }
        let start = Instant::now();
        e.cook(nodes, *t).unwrap();
        cpu.push(start.elapsed().as_secs_f64() * 1e3);
        match &timer {
            Some(timer) => gpu.push(timer.stop(e.device(), e.queue()).unwrap()),
            None => {
                let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
                gpu.push(f64::NAN);
            }
        }
        *t += 1.0 / 60.0;
    }
    [
        pct(&mut gpu, 0.5),
        pct(&mut gpu, 0.95),
        pct(&mut cpu, 0.5),
        pct(&mut cpu, 0.95),
    ]
}

fn created(e: &Engine) -> (usize, usize, usize) {
    (
        e.buffer_pool().created(),
        bind_groups_created_total(),
        e.pool().created(),
    )
}

#[test]
#[ignore = "spike: run with --release --ignored --nocapture"]
fn s1_two_million_sprites() {
    let Some(mut e) = engine(1920, 1080) else {
        return;
    };
    let mut edits = Vec::new();
    let grid = add(
        &mut e,
        &mut edits,
        "pnt.points",
        &[
            ("count", json!(2_000_000)),
            ("size", json!([3.5, 1.96, 0.0])),
            ("random_colour", json!(true)),
        ],
    );
    let draw = add(&mut e, &mut edits, "img.render_points", &[]);
    // An expression on time makes the draw cook every frame (the grid is
    // static, so it does not).
    expr(&mut edits, draw, "intensity", "0.3 + 0 * time");
    wire(&mut edits, grid, draw, "geo");
    e.edit("s1", edits).unwrap();
    let mut t = 0.0;
    for _ in 0..10 {
        e.cook(&[draw], t).unwrap();
        let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
        t += 1.0 / 60.0;
    }
    assert_eq!(e.error(draw), None);
    let before = created(&e);
    let [g50, g95, c50, c95] = measure(&mut e, &[draw], 200, &mut t);
    let after = created(&e);
    eprintln!(
        "S1: 2M sprites at 1920x1080: GPU p50 {g50:.3} ms, p95 {g95:.3} ms (target <= 3.0); \
         render-thread cook p50 {c50:.3} ms, p95 {c95:.3} ms (target <= 0.3); \
         created after frame 10 (buffers, bind groups, textures): {:?}",
        (after.0 - before.0, after.1 - before.1, after.2 - before.2)
    );
    assert_eq!(after, before, "nothing created after frame 10");
}

fn particles(e: &mut Engine) -> (NodeId, NodeId) {
    let mut edits = Vec::new();
    let sys = add(
        e,
        &mut edits,
        "pnt.particles",
        &[
            ("capacity", json!(2_000_000)),
            ("initial", json!(2_000_000)),
            ("rate", json!(500_000.0)),
            ("life", json!(4.0)),
            ("attract", json!(0.5)),
        ],
    );
    let draw = add(
        e,
        &mut edits,
        "img.render_points",
        &[("intensity", json!(0.3)), ("height", json!(3.0))],
    );
    wire(&mut edits, sys, draw, "geo");
    e.edit("s2", edits).unwrap();
    (sys, draw)
}

/// FNV-1a over bytes.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[test]
#[ignore = "spike: run with --release --ignored --nocapture"]
fn s2_two_million_particles() {
    let Some(mut e) = engine(1920, 1080) else {
        return;
    };
    let (sys, draw) = particles(&mut e);
    let mut t = 0.0;
    for _ in 0..30 {
        e.cook(&[draw], t).unwrap();
        let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
        t += 1.0 / 60.0;
    }
    assert_eq!(e.error(sys), None);
    let before = created(&e);
    let [s50, s95, sc50, _] = measure(&mut e, &[sys], 200, &mut t);
    let [f50, f95, fc50, fc95] = measure(&mut e, &[draw], 200, &mut t);
    let after = created(&e);
    eprintln!(
        "S2: 2M particles at 1920x1080: simulate GPU p50 {s50:.3} ms, p95 {s95:.3} ms (target <= 4.0); \
         simulate + draw GPU p50 {f50:.3} ms, p95 {f95:.3} ms (target <= 7.0), so draw ~{:.3} ms; \
         render-thread cook p50 {sc50:.3} ms (simulate), {fc50:.3} ms (both), p95 {fc95:.3} ms; \
         created after warm-up: {:?}",
        f50 - s50,
        (after.0 - before.0, after.1 - before.1, after.2 - before.2)
    );
    assert_eq!(after, before, "nothing created after warm-up");

    // Determinism: two offline runs of 300 frames, the second with every
    // core busy, give the same particles and the same frames.
    let run = |busy: bool| {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spinners: Vec<_> = (0..if busy { 32 } else { 0 })
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 1u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                        std::hint::black_box(x);
                    }
                })
            })
            .collect();
        let mut e = engine(1920, 1080).unwrap();
        e.set_mode(CookMode::Offline { fps: 60.0 });
        let (sys, draw) = particles(&mut e);
        let mut frames = Vec::new();
        let start = Instant::now();
        for f in 0..300u64 {
            e.set_frame(f);
            e.cook(&[draw, sys], f as f64 / 60.0).unwrap();
            let px =
                lsr_interop::readback::read_rgba8(e.device(), e.queue(), e.result(draw).unwrap())
                    .unwrap();
            frames.push(fnv(&px));
        }
        let g = e.geometry(sys).unwrap();
        let mut state = Vec::new();
        for name in ["P", "v", "age", "life", "id"] {
            let bytes = read_blocking(
                e.device(),
                e.queue(),
                e.buffer_pool(),
                g.point(name).unwrap(),
            )
            .unwrap();
            state.push(fnv(&bytes));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for s in spinners {
            s.join().unwrap();
        }
        eprintln!(
            "S2: 300 offline frames in {:.1} s (busy cores: {busy})",
            start.elapsed().as_secs_f64()
        );
        (frames, state)
    };
    let (a_frames, a_state) = run(false);
    let (b_frames, b_state) = run(true);
    let same = a_frames
        .iter()
        .zip(&b_frames)
        .filter(|(a, b)| a == b)
        .count();
    let distinct = a_frames
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    eprintln!(
        "S2: offline runs: {same}/300 frames identical ({distinct} distinct images); final P, v, age, life, id identical: {}",
        a_state == b_state
    );
    assert_eq!(a_frames, b_frames);
    assert_eq!(a_state, b_state);
}

#[test]
#[ignore = "spike: run with --release --ignored --nocapture"]
fn s3_thirty_deformers() {
    let Some(mut e) = engine(64, 64) else { return };
    let mut edits = Vec::new();
    let src = add(
        &mut e,
        &mut edits,
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(2_000_000))],
    );
    let mut last = src;
    for i in 0..30 {
        let n = add(
            &mut e,
            &mut edits,
            "pnt.noise",
            &[("amplitude", json!(0.01)), ("seed", json!(i))],
        );
        expr(&mut edits, n, "evolution", "time * 0.1");
        wire(&mut edits, last, n, "in");
        last = n;
    }
    e.edit("s3", edits).unwrap();
    let mut t = 0.0;
    for _ in 0..10 {
        e.cook(&[last], t).unwrap();
        let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
        t += 1.0 / 60.0;
    }
    let before = created(&e);
    let [g50, g95, c50, c95] = measure(&mut e, &[last], 100, &mut t);
    let after = created(&e);
    eprintln!(
        "S3: 30 noise deformers over 2M points, unfused (1 copy on write + 29 in place): \
         GPU p50 {g50:.3} ms, p95 {g95:.3} ms (fusion becomes an M2 task if > 3.0); \
         render-thread cook p50 {c50:.3} ms, p95 {c95:.3} ms; created after warm-up: {:?}; \
         pool holds {} MiB free",
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        e.buffer_pool().free_bytes() >> 20
    );
    assert_eq!(after, before);
}

/// A CPU operator in miniature (the MSH pattern of design §2.3): reads `P`
/// back without blocking, hands it to a worker thread that adds 1 to every
/// y and uploads the result itself (`Queue::write_buffer` from the worker),
/// and outputs the newest finished result. Records each result's latency in
/// frames.
struct RoundTrip {
    readback: Readback,
    /// Frame and geometry whose `P` is being read back.
    reading: Option<(u64, Geometry)>,
    /// Geometry whose `P` the worker is processing.
    working: Option<Geometry>,
    jobs: Option<mpsc::Sender<(u64, Mapped)>>,
    results: Option<mpsc::Receiver<(u64, Attribute)>>,
    latency: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
}

impl GeoOp for RoundTrip {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        if self.jobs.is_none() {
            let (jobs, rx) = mpsc::channel::<(u64, Mapped)>();
            let (tx, results) = mpsc::channel();
            let (pool, queue) = (ctx.buffers.clone(), ctx.queue.clone());
            std::thread::spawn(move || {
                for (frame, mapped) in rx {
                    // The copy out of the mapped buffer happens here, not on
                    // the render thread; dropping `mapped` unmaps it.
                    let mut bytes = mapped.bytes(0).unwrap();
                    drop(mapped);
                    for p in bytes.chunks_exact_mut(16) {
                        let y = f32::from_le_bytes(p[4..8].try_into().unwrap()) + 1.0;
                        p[4..8].copy_from_slice(&y.to_le_bytes());
                    }
                    let buffer = pool.attr(bytes.len() as u64);
                    queue.write_buffer(&buffer, 0, &bytes);
                    let n = (bytes.len() / 16) as u32;
                    if tx
                        .send((frame, Attribute::gpu("P", AttrType::VEC3, n, buffer)))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            self.jobs = Some(jobs);
            self.results = Some(results);
        }
        let mode = ctx.params.mode();
        // A finished result becomes the output.
        if self.working.is_some()
            && let Ok((frame, p)) = mode.receive(self.results.as_ref().unwrap())
        {
            let mut g = self.working.take().unwrap();
            g.set_point(p);
            *ctx.output = g;
            self.latency.lock().unwrap().push(ctx.frame - frame);
        }
        // A finished readback goes to the worker.
        if self.working.is_none()
            && self.reading.is_some()
            && let Some(mapped) = self.readback.take_mapped(mode, ctx.device)?
        {
            let (frame, g) = self.reading.take().unwrap();
            self.jobs.as_ref().unwrap().send((frame, mapped)).unwrap();
            self.working = Some(g);
        }
        // This frame's input starts its way to the CPU.
        if self.reading.is_none()
            && let Some(input) = ctx.input(0)
        {
            let g = input.clone();
            self.readback
                .record(ctx.encoder, ctx.buffers, g.point("P").ok_or("no P")?);
            self.reading = Some((ctx.frame, g));
        }
        Ok(())
    }

    fn after_submit(&mut self) {
        self.readback.start();
    }
}

#[test]
#[ignore = "spike: run with --release --ignored --nocapture"]
fn s3_cpu_round_trip() {
    let Some(mut e) = engine(64, 64) else { return };
    let latency = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let lat = latency.clone();
    e.extend(|r, l| {
        r.register(OperatorSchema {
            type_id: "pnt.round_trip".into(),
            family: OpFamily::Pnt,
            label: "Round Trip".into(),
            summary: "spike".into(),
            version: 1,
            params: vec![],
            inputs: vec![SocketSchema {
                name: "in".into(),
                wire: WireType::Geo,
                optional: true,
                feedback: false,
            }],
            outputs: vec![SocketSchema {
                name: "out".into(),
                wire: WireType::Geo,
                optional: false,
                feedback: false,
            }],
            capabilities: vec![],
            cook_budget_us: 500,
            time_dependent: true,
            sink: false,
        })
        .unwrap();
        l.add_geo("pnt.round_trip", move |_| {
            Box::new(RoundTrip {
                readback: Readback::new(),
                reading: None,
                working: None,
                jobs: None,
                results: None,
                latency: lat.clone(),
            })
        });
    });
    let mut edits = Vec::new();
    let src = add(
        &mut e,
        &mut edits,
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(100_000))],
    );
    // Zero amplitude but time-dependent: a fresh GPU-only P every frame.
    let n = add(
        &mut e,
        &mut edits,
        "pnt.noise",
        &[("amplitude", json!(0.0))],
    );
    expr(&mut edits, n, "evolution", "time");
    let rt = add(&mut e, &mut edits, "pnt.round_trip", &[]);
    wire(&mut edits, src, n, "in");
    wire(&mut edits, n, rt, "in");
    e.edit("s3b", edits).unwrap();
    // Live, paced at 60 fps like the app.
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let paced = |e: &mut Engine, nodes: &[NodeId], frames: u32| {
        let start = Instant::now();
        let mut cooks = Vec::new();
        for i in 0..frames {
            let t0 = Instant::now();
            e.cook(nodes, f64::from(i) / 60.0).unwrap();
            cooks.push(t0.elapsed().as_secs_f64() * 1e3);
            if let Some(wait) = (frame * (i + 1)).checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        cooks
    };
    // The same frames without the round trip, for comparison.
    let mut base = paced(&mut e, &[n, src], 180)[10..].to_vec();
    let (b50, b95, bmax) = (
        pct(&mut base, 0.5),
        pct(&mut base, 0.95),
        pct(&mut base, 1.0),
    );
    eprintln!(
        "S3: baseline (points + noise, no round trip): cook p50 {b50:.3} ms, p95 {b95:.3} ms, max {bmax:.3} ms"
    );
    let cooks = paced(&mut e, &[rt, src], 180);
    assert_eq!(e.error(rt), None);
    let lat = latency.lock().unwrap().clone();
    let skip = 10;
    let cooks_after = &mut cooks[skip..].to_vec();
    let (c50, c95, cmax) = (
        pct(cooks_after, 0.5),
        pct(cooks_after, 0.95),
        cooks_after[cooks_after.len() - 1],
    );
    let (lmin, lmax) = (lat.iter().min().copied(), lat.iter().max().copied());
    // Values: y + 1 of the source.
    let want = read_blocking(
        e.device(),
        e.queue(),
        e.buffer_pool(),
        e.geometry(src).unwrap().point("P").unwrap(),
    )
    .unwrap();
    let got = read_blocking(
        e.device(),
        e.queue(),
        e.buffer_pool(),
        e.geometry(rt).unwrap().point("P").unwrap(),
    )
    .unwrap();
    let ok = want
        .chunks_exact(16)
        .zip(got.chunks_exact(16))
        .all(|(w, g)| {
            let f =
                |b: &[u8], i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
            f(g, 0) == f(w, 0) && f(g, 1) == f(w, 1) + 1.0 && f(g, 2) == f(w, 2)
        });
    eprintln!(
        "S3: 100k-point GPU -> CPU worker -> GPU round trip, live at 60 fps: {} results, latency {lmin:?}..{lmax:?} frames; \
         render-thread cook (whole frame, 3 nodes) p50 {c50:.3} ms, p95 {c95:.3} ms, max {cmax:.3} ms; \
         values y+1: {ok}; write_buffer from the worker thread: works",
        lat.len()
    );
    assert!(ok, "the worker's upload has the expected values");
    assert!(lat.len() > 30, "results keep arriving");
}
