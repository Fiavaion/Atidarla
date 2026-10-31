//! MSH nodes in the engine: worker cooks, hand-back with GPU upload, PNT
//! inputs read back from the GPU, live (never waits) and offline (waits)
//! behaviour, caching. Skips without a GPU unless `ATD_REQUIRE_GPU` is set.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::Residency;
use atd_gpu::geo::readback::read_blocking;
use serde_json::{Value, json};

/// WARP and some drivers crash with several devices running compute in
/// parallel: one GPU test at a time.
static GPU: Mutex<()> = Mutex::new(());

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()?;
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    atd_ops_msh::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    atd_ops_msh::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 64,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

struct Build<'a> {
    e: &'a mut Engine,
    edits: Vec<Edit>,
}

impl<'a> Build<'a> {
    fn new(e: &'a mut Engine) -> Self {
        Self {
            e,
            edits: Vec::new(),
        }
    }

    fn node(&mut self, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let id = self.e.alloc_id();
        self.edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", id.0)),
        });
        for (p, v) in params {
            self.param(id, p, v.clone());
        }
        id
    }

    fn param(&mut self, node: NodeId, p: &str, v: Value) {
        self.edits.push(Edit::SetParam {
            node,
            param: p.into(),
            value: Some(Param::constant(v)),
        });
    }

    fn expr(&mut self, node: NodeId, param: &str, expr: &str) {
        self.edits.push(Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param {
                value: json!(0),
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

    fn done(self) {
        self.e.edit("build", self.edits).unwrap();
    }
}

fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<[f32; 4]> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    read_blocking(e.device(), e.queue(), e.buffer_pool(), a)
        .unwrap()
        .chunks_exact(16)
        .map(|c| {
            std::array::from_fn(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))
        })
        .collect()
}

/// Cooks live, paced, until nothing is pending (or a deadline).
fn settle(e: &mut Engine, nodes: &[NodeId], t: &mut f64) -> usize {
    let start = Instant::now();
    let mut frames = 0;
    loop {
        e.cook(nodes, *t).unwrap();
        *t += 1.0 / 60.0;
        frames += 1;
        if !e.pending() || start.elapsed() > Duration::from_secs(20) {
            return frames;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn live_cook_never_waits_and_delivers_on_the_gpu() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let s = b.node("msh.sphere", &[("rows", json!(32)), ("columns", json!(64))]);
    let r = b.node("img.render_points", &[]);
    b.wire(s, r, "geo");
    b.done();
    // The first cook only hands the job over: no result yet, no error.
    e.cook(&[r], 0.0).unwrap();
    assert_eq!(e.error(s), None);
    assert_eq!(e.geometry(s).map_or(0, |g| g.points()), 0);
    let mut t = 0.0;
    settle(&mut e, &[r], &mut t);
    assert_eq!(e.error(s), None);
    let g = e.geometry(s).unwrap();
    assert_eq!(g.points(), 31 * 64 + 2);
    assert_eq!(g.prims(), 32 * 64);
    assert_eq!(
        g.point("P").unwrap().residency(),
        Residency::Both,
        "uploaded by the worker"
    );
    assert!(g.header().is_some());
    assert!(
        e.status(s).unwrap().starts_with("cooked in"),
        "{:?}",
        e.status(s)
    );
    // Unchanged: no new job, the same value.
    let before = e.geometry(s).unwrap().point("P").unwrap().clone();
    e.cook(&[r], t).unwrap();
    assert!(!e.pending());
    assert!(e.geometry(s).unwrap().point("P").unwrap().shares(&before));
}

#[test]
fn offline_waits_for_worker_and_gpu_inputs() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = engine() else { return };
    e.set_mode(CookMode::Offline { fps: 30.0 });
    let mut b = Build::new(&mut e);
    // A GPU-only PNT source moved by time feeds an MSH transform.
    let pts = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(5)),
            ("size", json!([4.0, 0.0, 0.0])),
        ],
    );
    let tr = b.node("msh.transform", &[]);
    b.expr(tr, "uniform", "1 + frame");
    b.wire(pts, tr, "in");
    let bx = b.node("msh.box", &[]);
    let noise = b.node("pnt.noise", &[("amplitude", json!(0.0))]);
    b.wire(bx, noise, "in");
    b.done();
    for frame in 0..3u32 {
        e.cook(&[tr, noise], f64::from(frame) / 30.0).unwrap();
        assert_eq!(e.error(tr), None);
        let p = floats(&e, tr, "P");
        assert_eq!(p.len(), 5);
        // Frame i uses frame i's parameters: x = -2 × (1 + i) at the start.
        assert_eq!(p[0][0], -2.0 * (1.0 + frame as f32), "frame {frame}");
        assert!(!e.pending(), "offline leaves nothing in flight");
    }
    // An MSH result feeds a GPU deformer (it needs the header).
    assert_eq!(e.error(noise), None);
    assert_eq!(floats(&e, noise, "P").len(), 8);
}

/// PNT Resample (cooked here, PNT by type ID) between an MSH curve and a
/// GPU→SIG readback: offline, each frame's curve is resampled and read
/// back in the same frame.
#[test]
fn resample_respaces_a_curve_and_feeds_to_channels_offline() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = engine() else { return };
    e.set_mode(CookMode::Offline { fps: 30.0 });
    let mut b = Build::new(&mut e);
    let line = b.node(
        "msh.line",
        &[("end", json!([2.0, 0.0, 0.0])), ("points", json!(2))],
    );
    let tr = b.node("msh.transform", &[]);
    b.expr(tr, "uniform", "1 + frame");
    let rs = b.node(
        "pnt.resample",
        &[("mode", json!("count")), ("count", json!(5))],
    );
    let ch = b.node("pnt.to_channels", &[("layout", json!("samples"))]);
    b.wire(line, tr, "in");
    b.wire(tr, rs, "in");
    b.wire(rs, ch, "in");
    b.done();
    for frame in 0..3u32 {
        e.cook(&[ch], f64::from(frame) / 30.0).unwrap();
        assert_eq!(e.error(rs), None);
        assert_eq!(e.error(ch), None);
        let g = e.geometry(rs).unwrap();
        assert_eq!((g.points(), g.prims()), (5, 1));
        let c = e.channels(ch).unwrap();
        assert_eq!(c.name(0), "P_x");
        let length = 2.0 * (1.0 + frame as f32);
        let want: Vec<f32> = (0..5).map(|k| length * k as f32 / 4.0).collect();
        assert_eq!(c.channel(0), want.as_slice(), "frame {frame}");
        assert!(c.channel(1).iter().all(|&y| y == 0.0));
    }
}

#[test]
fn gpu_inputs_are_read_back_live() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node(
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(1000))],
    );
    let fuse = b.node("msh.fuse", &[("distance", json!(0.0))]);
    b.wire(pts, fuse, "in");
    b.done();
    let mut t = 0.0;
    let frames = settle(&mut e, &[fuse], &mut t);
    assert!(frames >= 2, "a readback takes at least a frame: {frames}");
    assert_eq!(e.error(fuse), None);
    assert_eq!(e.geometry(fuse).unwrap().points(), 1000);
    assert_eq!(floats(&e, fuse, "P"), floats(&e, pts, "P"));
}

#[test]
fn errors_are_reported_and_recover() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let bx = b.node("msh.box", &[]);
    let del = b.node("msh.delete", &[("group", json!("missing"))]);
    b.wire(bx, del, "in");
    b.done();
    let mut t = 0.0;
    settle(&mut e, &[del], &mut t);
    assert!(
        e.error(del).is_some_and(|m| m.contains("missing")),
        "{:?}",
        e.error(del)
    );
    let mut b = Build::new(&mut e);
    b.param(del, "group", json!(""));
    b.done();
    settle(&mut e, &[del], &mut t);
    assert_eq!(e.error(del), None);
    assert_eq!(e.geometry(del).unwrap().points(), 8);
}

fn pixels(e: &Engine, node: NodeId) -> Vec<u8> {
    assert_eq!(e.error(node), None);
    atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()
}

/// M2 AC5: scatter (MSH) → Iso Surface (MSH) → noise deform (PNT) → Render
/// Points, wired GEO to GEO with no conversion node, offline twice with the
/// same bytes.
#[test]
fn pnt_msh_pnt_chain_wires_without_conversion() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let mut runs = Vec::new();
    for _ in 0..2 {
        let Some(mut e) = engine() else { return };
        e.set_mode(CookMode::Offline { fps: 30.0 });
        let mut b = Build::new(&mut e);
        let src = b.node("msh.sphere", &[("radius", json!(0.6))]);
        let sc = b.node("msh.scatter", &[("count", json!(400)), ("seed", json!(3))]);
        let iso = b.node(
            "msh.iso_surface",
            &[("radius", json!(0.08)), ("resolution", json!(48))],
        );
        let noise = b.node("pnt.noise", &[("amplitude", json!(0.05))]);
        b.expr(noise, "evolution", "time");
        let draw = b.node("img.render_points", &[("size", json!(1.0))]);
        b.wire(src, sc, "in");
        b.wire(sc, iso, "in");
        b.wire(iso, noise, "in");
        b.wire(noise, draw, "geo");
        b.done();
        let mut frames = Vec::new();
        for f in 0..3u32 {
            e.cook(&[draw], f64::from(f) / 30.0).unwrap();
            for n in [sc, iso, noise] {
                assert_eq!(e.error(n), None);
            }
            frames.push(pixels(&e, draw));
        }
        let g = e.geometry(noise).unwrap();
        let iso_g = e.geometry(iso).unwrap();
        assert_eq!(g.points(), iso_g.points());
        assert!(
            g.prims() > 100 && g.prims() == iso_g.prims(),
            "topology rides along"
        );
        // The deformer wrote P on the GPU (copy on write), the mesh stays.
        assert!(!g.point("P").unwrap().shares(iso_g.point("P").unwrap()));
        let lit = frames[2].chunks_exact(4).filter(|p| p[0] > 0).count();
        assert!(lit > 100, "{lit} lit pixels");
        assert_ne!(frames[0], frames[2], "the noise moves with time");
        runs.push(frames);
    }
    if runs.len() == 2 {
        assert_eq!(runs[0], runs[1], "two offline runs give the same bytes");
    }
}

/// Live, a parameter that changes every frame: stale jobs are dropped or
/// cancelled, results keep arriving, and the final result is the newest.
#[test]
fn latest_parameters_win_live() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let iso = b.node(
        "msh.iso_surface",
        &[("source", json!("sphere")), ("resolution", json!(96))],
    );
    b.done();
    let mut t = 0.0;
    for f in 0..40 {
        let mut b = Build::new(&mut e);
        b.param(iso, "radius", json!(0.1 + 0.01 * f64::from(f)));
        b.done();
        e.cook(&[iso], t).unwrap();
        t += 1.0 / 60.0;
        assert_eq!(e.error(iso), None);
    }
    settle(&mut e, &[iso], &mut t);
    let p = floats(&e, iso, "P");
    let r = p
        .iter()
        .map(|q| (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt())
        .fold(0.0f32, f32::max);
    assert!((r - 0.49).abs() < 0.02, "the last radius (0.49) wins: {r}");
}
