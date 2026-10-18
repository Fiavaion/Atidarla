//! PNT operators and Render Points on the GPU: known points give known
//! pixels; particles are born, age, die in order and repeat exactly
//! offline. Skips without a GPU unless `ATD_REQUIRE_GPU` is set.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::{read_blocking, read_buffer_blocking};
use serde_json::{Value, json};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()
}

fn engine(size: u32) -> Option<Engine> {
    let (device, queue) = gpu()?;
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: size,
        height: size,
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
            self.edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
        id
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

/// Point attribute `name` of `node` as rows of `comps` floats (stride 4
/// words for 3 and 4 components).
fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<Vec<f32>> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    let bytes = read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap();
    let stride = a.ty().stride() as usize;
    let comps = a.ty().comps() as usize;
    bytes
        .chunks_exact(stride)
        .map(|c| {
            (0..comps)
                .map(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))
                .collect()
        })
        .collect()
}

fn words(e: &Engine, node: NodeId, name: &str) -> Vec<u32> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    read_blocking(e.device(), e.queue(), e.buffer_pool(), a)
        .unwrap()
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// The live count from a geometry's GPU header.
fn count(e: &Engine, node: NodeId) -> u32 {
    let g = e.geometry(node).expect("geometry");
    let h = read_buffer_blocking(e.device(), e.queue(), g.header().unwrap(), 48).unwrap();
    u32::from_le_bytes(h[..4].try_into().unwrap())
}

fn pixels(e: &Engine, node: NodeId) -> Vec<[u8; 4]> {
    assert_eq!(e.error(node), None);
    atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(node).unwrap())
        .unwrap()
        .chunks_exact(4)
        .map(|c| c.try_into().unwrap())
        .collect()
}

#[test]
fn points_make_grids_lines_circles_and_seeded_random_boxes() {
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = b.node(
        "pnt.points",
        &[
            ("count", json!(4)),
            ("size", json!([2.0, 4.0, 0.0])),
            ("centre", json!([10.0, 0.0, 1.0])),
            ("colour", json!([0.25, 0.5, 0.75, 1.0])),
        ],
    );
    let line = b.node(
        "pnt.points",
        &[("shape", json!("line")), ("count", json!(3))],
    );
    let circle = b.node(
        "pnt.points",
        &[("shape", json!("circle")), ("count", json!(4))],
    );
    let rand_a = b.node(
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(100)),
            ("seed", json!(3)),
        ],
    );
    let rand_b = b.node(
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(100)),
            ("seed", json!(3)),
        ],
    );
    let rand_c = b.node(
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(100)),
            ("seed", json!(4)),
        ],
    );
    let empty = b.node("pnt.points", &[("count", json!(0))]);
    b.done();
    let all = [grid, line, circle, rand_a, rand_b, rand_c, empty];
    e.cook(&all, 0.0).unwrap();
    for n in all {
        assert_eq!(e.error(n), None);
    }
    // A 2 × 2 grid, 2 wide and 4 tall, about (10, 0, 1).
    assert_eq!(
        floats(&e, grid, "P"),
        [
            [9.0, -2.0, 1.0],
            [11.0, -2.0, 1.0],
            [9.0, 2.0, 1.0],
            [11.0, 2.0, 1.0]
        ]
    );
    assert!(
        floats(&e, grid, "Cd")
            .iter()
            .all(|c| c == &[0.25, 0.5, 0.75, 1.0])
    );
    assert_eq!(words(&e, grid, "id"), [0, 1, 2, 3]);
    assert_eq!(
        floats(&e, line, "P"),
        [[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
    );
    let c = floats(&e, circle, "P");
    for (got, want) in c
        .iter()
        .zip([[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]])
    {
        assert!(
            (got[0] - want[0]).abs() < 1e-6 && (got[1] - want[1]).abs() < 1e-6,
            "{c:?}"
        );
    }
    let (a, bb, cc) = (
        floats(&e, rand_a, "P"),
        floats(&e, rand_b, "P"),
        floats(&e, rand_c, "P"),
    );
    assert_eq!(a, bb, "the same seed gives the same points");
    assert_ne!(a, cc);
    assert!(
        a.iter().flatten().all(|v| (-1.0..=1.0).contains(v)),
        "inside the box"
    );
    assert_eq!(e.geometry(empty).unwrap().points(), 0);
    // Cached: nothing cooks again.
    assert_eq!(e.cook(&all, 0.1).unwrap(), 0);
}

#[test]
fn noise_displaces_or_accelerates_and_repeats() {
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let src = b.node(
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(500))],
    );
    let still = b.node("pnt.noise", &[("amplitude", json!(0.0))]);
    let moved = b.node("pnt.noise", &[("amplitude", json!(0.3))]);
    let moved2 = b.node("pnt.noise", &[("amplitude", json!(0.3))]);
    let vel = b.node(
        "pnt.noise",
        &[("mode", json!("velocity")), ("amplitude", json!(2.0))],
    );
    for n in [still, moved, moved2, vel] {
        b.wire(src, n, "in");
    }
    b.done();
    e.set_mode(CookMode::Offline { fps: 50.0 });
    let all = [src, still, moved, moved2, vel];
    e.cook(&all, 0.0).unwrap();
    for n in all {
        assert_eq!(e.error(n), None);
    }
    let p0 = floats(&e, src, "P");
    assert_eq!(floats(&e, still, "P"), p0, "zero amplitude is the identity");
    let p1 = floats(&e, moved, "P");
    assert_eq!(floats(&e, moved2, "P"), p1, "deterministic");
    let d: Vec<f32> = p0
        .iter()
        .zip(&p1)
        .map(|(a, b)| {
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .collect();
    assert!(
        d.iter().all(|d| *d < 0.3 * 10.0),
        "bounded by the amplitude"
    );
    assert!(d.iter().sum::<f32>() / d.len() as f32 > 0.01, "points move");
    // Velocity mode leaves P alone and makes v = a × dt (v was missing).
    assert_eq!(floats(&e, vel, "P"), p0);
    let v = floats(&e, vel, "v");
    assert_eq!(v.len(), 500);
    let speed = v
        .iter()
        .map(|v| v.iter().map(|c| c * c).sum::<f32>().sqrt())
        .fold(0.0, f32::max);
    assert!(speed > 0.0 && speed < 2.0 * 10.0 / 50.0, "{speed}");
    // Shared attributes are not copied.
    let g = e.geometry(moved).unwrap();
    assert!(
        g.point("Cd")
            .unwrap()
            .shares(e.geometry(src).unwrap().point("Cd").unwrap())
    );
}

/// Render Points: known points light exactly the pixels they cover (2-px
/// sprites centred on pixel corners), additively.
#[test]
fn render_points_lights_known_pixels() {
    let Some(mut e) = engine(16) else { return };
    let mut b = Build::new(&mut e);
    // Two points at (-0.5, 0.5) and (0.25, 0.5): pixel corners (4, 4) and
    // (10, 4) of a 16 × 16 image showing [-1, 1]².
    let two = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(2)),
            ("size", json!([0.75, 0.0, 0.0])),
            ("centre", json!([-0.125, 0.5, 0.0])),
        ],
    );
    let r = b.node(
        "img.render_points",
        &[
            ("colour_from", json!("constant")),
            ("colour", json!([1.0, 0.2, 0.0, 1.0])),
        ],
    );
    b.wire(two, r, "geo");
    // Two points on the same spot (-0.5, -0.5) add up; colour from `Cd`.
    let same = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(2)),
            ("size", json!([0.0, 0.0, 0.0])),
            ("centre", json!([-0.5, -0.5, 0.0])),
            ("colour", json!([0.2, 0.0, 1.0, 1.0])),
        ],
    );
    let r2 = b.node("img.render_points", &[]);
    b.wire(same, r2, "geo");
    b.done();
    e.cook(&[r, r2], 0.0).unwrap();
    let px = pixels(&e, r);
    for y in 0..16 {
        for x in 0..16 {
            let lit = (3..=4).contains(&y) && ((3..=4).contains(&x) || (9..=10).contains(&x));
            let want = if lit {
                [255, 51, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(px[y * 16 + x], want, "pixel ({x}, {y})");
        }
    }
    let px = pixels(&e, r2);
    for y in 0..16 {
        for x in 0..16 {
            let lit = (11..=12).contains(&y) && (3..=4).contains(&x);
            let want = if lit {
                [102, 0, 255, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(px[y * 16 + x], want, "pixel ({x}, {y})");
        }
    }
}

/// Particles under a fixed step: 10 born per frame, each lives exactly 30
/// frames, survivors keep their order (ids ascend), the capacity clamps.
#[test]
fn particles_are_born_age_and_die_in_order() {
    let Some(mut e) = engine(8) else { return };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let mut b = Build::new(&mut e);
    let p = b.node(
        "pnt.particles",
        &[
            ("capacity", json!(1000)),
            ("rate", json!(600.0)),
            ("life", json!(30.5 / 60.0)),
            ("life_variance", json!(0.0)),
        ],
    );
    let full = b.node(
        "pnt.particles",
        &[
            ("capacity", json!(100)),
            ("initial", json!(250)),
            ("rate", json!(0.0)),
        ],
    );
    b.done();
    for f in 0..45u64 {
        e.set_frame(f);
        e.cook(&[p, full], f as f64 / 60.0).unwrap();
        assert_eq!(e.error(p), None);
        let n = count(&e, p);
        assert_eq!(n, (10 * (f as u32 + 1)).min(300), "frame {f}");
        if f == 0 {
            assert_eq!(count(&e, full), 100, "the burst is clamped to the capacity");
        }
    }
    let ids = words(&e, p, "id");
    let n = count(&e, p) as usize;
    assert!(
        ids[..n].windows(2).all(|w| w[0] < w[1]),
        "order kept: {:?}",
        &ids[..n]
    );
    assert_eq!(
        (ids[0], ids[n - 1]),
        (150, 449),
        "the oldest alive and the newest"
    );
    let left = count(&e, full);
    assert!(left > 0 && left < 100, "prewarmed ages: some died ({left})");
    let ages = floats(&e, p, "age");
    assert!((ages[0][0] - 30.0 / 60.0).abs() < 1e-4, "{:?}", ages[0]);
    assert!(
        (ages[n - 1][0] - 1.0 / 60.0).abs() < 1e-6,
        "{:?}",
        ages[n - 1]
    );
}

fn particle_project(e: &mut Engine, chain: bool) -> (NodeId, NodeId) {
    let mut b = Build::new(e);
    let p = b.node(
        "pnt.particles",
        &[
            ("capacity", json!(20_000)),
            ("rate", json!(30_000.0)),
            ("initial", json!(5000)),
            ("life", json!(0.4)),
            ("attract", json!(1.0)),
        ],
    );
    let r = b.node("img.render_points", &[("height", json!(3.0))]);
    if chain {
        let n = b.node(
            "pnt.noise",
            &[("mode", json!("velocity")), ("amplitude", json!(5.0))],
        );
        b.expr(n, "seed", "frame % 3");
        b.wire(p, n, "in");
        b.wire(n, p, "prev");
        b.wire(n, r, "geo");
    } else {
        b.wire(p, r, "geo");
    }
    b.done();
    (p, r)
}

/// Offline, two runs give the same particles and the same frames.
#[test]
fn particles_repeat_exactly_offline() {
    let run = |chain: bool| {
        let mut e = engine(64)?;
        e.set_mode(CookMode::Offline { fps: 60.0 });
        let (p, r) = particle_project(&mut e, chain);
        let mut frames = Vec::new();
        for f in 0..40u64 {
            e.set_frame(f);
            e.cook(&[r], f as f64 / 60.0).unwrap();
            assert_eq!(e.error(p), None);
            frames.push(pixels(&e, r));
        }
        Some(frames)
    };
    let Some(a) = run(false) else { return };
    let b = run(false).unwrap();
    assert_eq!(a, b, "byte-identical frames");
    assert_ne!(a[5], a[30], "the picture changes");
    let c = run(true).unwrap();
    let d = run(true).unwrap();
    assert_eq!(c, d, "a feedback chain is deterministic too");
    assert_ne!(a[30], c[30], "the noise in the loop changes the motion");
}

/// The feedback chain carries the downstream force to the next frame: the
/// system's own count is unchanged by it.
#[test]
fn particles_take_their_state_from_a_feedback_chain() {
    let Some(mut e) = engine(32) else { return };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let (p, r) = particle_project(&mut e, true);
    let Some(mut e2) = engine(32) else { return };
    e2.set_mode(CookMode::Offline { fps: 60.0 });
    let (p2, r2) = particle_project(&mut e2, false);
    // Requesting the system too keeps its own result readable (otherwise its
    // value is handed over to the noise, its only reader).
    for f in 0..20u64 {
        for (e, r, p) in [(&mut e, r, p), (&mut e2, r2, p2)] {
            e.set_frame(f);
            e.cook(&[r, p], f as f64 / 60.0).unwrap();
            assert_eq!(e.error(p), None);
        }
    }
    assert_eq!(
        count(&e, p),
        count(&e2, p2),
        "births and deaths do not depend on forces"
    );
    assert_ne!(floats(&e, p, "P"), floats(&e2, p2, "P"));
}
