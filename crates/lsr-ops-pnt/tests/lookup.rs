//! The M6 per-point attribute operators on the GPU: Attribute Noise,
//! Attribute from Channels and Falloff Field, checked against the same
//! arithmetic on the CPU. Skips without a GPU unless `LSR_REQUIRE_GPU` is
//! set.

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use lsr_gpu::geo::readback::read_blocking;
use serde_json::{Value, json};

fn engine() -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: lsr_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()?;
    let mut reg = Registry::default();
    lsr_ops_pnt::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    lsr_ops_msh::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_pnt::library(&mut lib);
    lsr_ops_sig::library(&mut lib);
    lsr_ops_msh::library(&mut lib);
    let config = EngineConfig {
        width: 16,
        height: 16,
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

    fn wire(&mut self, from: NodeId, to: NodeId, socket: &str) {
        self.edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
        });
    }

    fn done(self) {
        self.e.edit("build", self.edits).unwrap();
    }
}

fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<Vec<f32>> {
    assert_eq!(e.error(node), None);
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

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[test]
fn falloff_fields_weight_points_by_shape_and_layer() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let line = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(41)),
            ("size", json!([4.0, 0.0, 0.0])),
        ],
    );
    let sphere = b.node(
        "pnt.falloff_field",
        &[("radius", json!(1.0)), ("inner", json!(0.5))],
    );
    let plane = b.node(
        "pnt.falloff_field",
        &[
            ("shape", json!("plane")),
            ("direction", json!([1.0, 0.0, 0.0])),
            ("centre", json!([1.0, 0.0, 0.0])),
            ("radius", json!(0.0)),
            ("operation", json!("max")),
        ],
    );
    let boxed = b.node(
        "pnt.falloff_field",
        &[
            ("name", json!("inside")),
            ("shape", json!("box")),
            ("size", json!([1.0, 1.0, 1.0])),
            ("radius", json!(1.0)),
            ("curve", json!("linear")),
            ("invert", json!(true)),
        ],
    );
    b.wire(line, sphere, "in");
    b.wire(sphere, plane, "in");
    b.wire(plane, boxed, "in");
    b.done();
    e.cook(&[boxed], 0.0).unwrap();
    let p = floats(&e, boxed, "P");
    let w = floats(&e, boxed, "falloff");
    let inside = floats(&e, boxed, "inside");
    for i in 0..41 {
        let x = p[i][0];
        let sphere = if x.abs() <= 0.5 {
            1.0
        } else {
            smoothstep(1.0 - (x.abs() - 0.5) / 0.5)
        };
        // The half-space x >= 1 has weight 1 (max with the sphere).
        let expected = if x >= 1.0 { 1.0f32 } else { sphere };
        assert!(
            (w[i][0] - expected).abs() < 1e-5,
            "x {x}: {} vs {expected}",
            w[i][0]
        );
        let outside = (x.abs() - 0.5).max(0.0);
        let boxed = 1.0 - (1.0 - outside).clamp(0.0, 1.0);
        assert!((inside[i][0] - boxed).abs() < 1e-5, "x {x}");
    }
}

#[test]
fn attribute_noise_adds_seeded_fractal_noise_in_range() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node("pnt.points", &[("count", json!(400))]);
    let n1 = b.node(
        "pnt.attribute_noise",
        &[
            ("name", json!("w")),
            ("type", json!("float")),
            ("operation", json!("set")),
            ("amplitude", json!(0.5)),
            ("centre", json!(0.5)),
        ],
    );
    let n2 = b.node(
        "pnt.attribute_noise",
        &[
            ("name", json!("w")),
            ("type", json!("float")),
            ("operation", json!("set")),
            ("amplitude", json!(0.5)),
            ("centre", json!(0.5)),
        ],
    );
    let curl = b.node(
        "pnt.attribute_noise",
        &[
            ("name", json!("v")),
            ("type", json!("vec3")),
            ("noise", json!("curl")),
        ],
    );
    b.wire(pts, n1, "in");
    b.wire(pts, n2, "in");
    b.wire(pts, curl, "in");
    b.done();
    e.cook(&[n1, n2, curl], 0.0).unwrap();
    let a = floats(&e, n1, "w");
    let b2 = floats(&e, n2, "w");
    assert!(a == b2, "same seed and parameters, same values");
    let (lo, hi) = a
        .iter()
        .fold((1.0f32, 0.0f32), |(l, h), v| (l.min(v[0]), h.max(v[0])));
    assert!(lo >= 0.0 && hi <= 1.0, "{lo}..{hi}");
    assert!(hi - lo > 0.2, "the noise varies: {lo}..{hi}");
    let v = floats(&e, curl, "v");
    assert!(v.iter().any(|v| v.iter().any(|c| c.abs() > 1e-3)));
    assert!(
        v.iter()
            .all(|v| v.len() == 3 && v.iter().all(|c| c.is_finite()))
    );
}

#[test]
fn attribute_from_channels_reads_samples_channels_and_positions() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node(
        "pnt.points",
        &[("shape", json!("line")), ("count", json!(8))],
    );
    let wave = b.node(
        "sig.pattern",
        &[("shape", json!("ramp")), ("length", json!(4))],
    );
    let bank = b.node(
        "sig.constant",
        &[
            ("value1", json!(0.25)),
            ("name2", json!("b")),
            ("value2", json!(0.5)),
            ("name3", json!("c")),
            ("value3", json!(0.75)),
        ],
    );
    let by_index = b.node("pnt.attribute_from_channels", &[("name", json!("s"))]);
    let by_channel = b.node(
        "pnt.attribute_from_channels",
        &[
            ("name", json!("c")),
            ("lookup", json!("channels")),
            ("wrap", json!(true)),
            ("scale", json!(2.0)),
        ],
    );
    let u = b.node(
        "pnt.attribute",
        &[
            ("name", json!("u")),
            ("source", json!("index")),
            ("value", json!([0.0, 0.0, 0.0, 0.0])),
            ("value2", json!([1.0, 0.0, 0.0, 0.0])),
        ],
    );
    let by_attr = b.node(
        "pnt.attribute_from_channels",
        &[("name", json!("t")), ("lookup", json!("attribute"))],
    );
    b.wire(pts, by_index, "in");
    b.wire(wave, by_index, "channels");
    b.wire(pts, by_channel, "in");
    b.wire(bank, by_channel, "channels");
    b.wire(pts, u, "in");
    b.wire(u, by_attr, "in");
    b.wire(wave, by_attr, "channels");
    b.done();
    e.cook(&[by_index, by_channel, by_attr], 0.0).unwrap();
    let ramp: Vec<f32> = e.channels(wave).unwrap().channel(0).to_vec();
    assert_eq!(ramp.len(), 4);
    let s = floats(&e, by_index, "s");
    for i in 0..8 {
        assert_eq!(s[i][0], ramp[i.min(3)], "held past the last sample");
    }
    let c = floats(&e, by_channel, "c");
    for i in 0..8 {
        assert_eq!(c[i][0], [0.5, 1.0, 1.5][i % 3], "channel i, wrapped, × 2");
    }
    let t = floats(&e, by_attr, "t");
    for (i, t) in t.iter().enumerate() {
        let x = i as f32 / 7.0 * 3.0;
        let (a, f) = (x.floor() as usize, x.fract());
        let want = ramp[a] + (ramp[(a + 1).min(3)] - ramp[a]) * f;
        assert!((t[0] - want).abs() < 1e-5, "{} vs {want}", t[0]);
    }
}

/// A signed-integer attribute's words.
fn ints(e: &Engine, node: NodeId, name: &str) -> Vec<i32> {
    assert_eq!(e.error(node), None);
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    read_blocking(e.device(), e.queue(), e.buffer_pool(), a)
        .unwrap()
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// A line of `n` points from x = −1 to 1.
fn line(b: &mut Build<'_>, n: u32) -> NodeId {
    b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(n)),
            ("size", json!([2.0, 0.0, 0.0])),
        ],
    )
}

#[test]
fn proximity_and_neighbours_find_the_nearest_points() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let targets = line(&mut b, 3);
    let probes = line(&mut b, 21);
    let lifted = b.node("pnt.transform", &[("translate", json!([0.0, 0.3, 0.0]))]);
    let prox = b.node("pnt.proximity", &[("radius", json!(1.5))]);
    let own = b.node("pnt.proximity", &[("radius", json!(0.5))]);
    let knn = b.node(
        "pnt.neighbours",
        &[("k", json!(2)), ("radius", json!(0.45))],
    );
    let spaced = line(&mut b, 11);
    b.wire(probes, lifted, "in");
    b.wire(lifted, prox, "in");
    b.wire(targets, prox, "to");
    b.wire(spaced, own, "in");
    b.wire(spaced, knn, "in");
    b.done();
    e.cook(&[prox, own, knn], 0.0).unwrap();
    let p = floats(&e, prox, "P");
    let d = floats(&e, prox, "dist");
    let v = floats(&e, prox, "nearvec");
    for i in 0..21 {
        let x = p[i][0];
        let near = [-1.0f32, 0.0, 1.0]
            .into_iter()
            .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
            .unwrap();
        let want = ((near - x).powi(2) + 0.09).sqrt();
        assert!(
            (d[i][0] - want).abs() < 1e-5,
            "x {x}: {} vs {want}",
            d[i][0]
        );
        assert!((v[i][0] - (near - x)).abs() < 1e-5 && (v[i][1] + 0.3).abs() < 1e-5);
    }
    // Within one cloud a point is not its own nearest: the spacing.
    for row in floats(&e, own, "dist") {
        assert!((row[0] - 0.2).abs() < 1e-5, "{row:?}");
    }
    let nbr = ints(&e, knn, "nbr");
    let count = ints(&e, knn, "nbrcount");
    let nd = floats(&e, knn, "nbrdist");
    for i in 0..11i32 {
        let n = &nbr[i as usize * 4..i as usize * 4 + 4];
        let want = match i {
            0 => [1, 2],
            10 => [9, 8],
            _ => [i - 1, i + 1],
        };
        // The two sides are equally near up to rounding: either order.
        let mut got = [n[0], n[1]];
        let mut want = want;
        if i != 0 && i != 10 {
            got.sort();
            want.sort();
        }
        assert_eq!(got, want, "point {i}");
        assert!(nd[i as usize][0] <= nd[i as usize][1], "nearest first");
        assert_eq!(&n[2..], &[-1, -1], "k = 2");
        assert!((nd[i as usize][0] - 0.2).abs() < 1e-5);
        let within = (0..11).filter(|j| *j != i && (j - i).abs() <= 2).count() as i32;
        assert_eq!(count[i as usize], within, "point {i}");
    }
}

#[test]
fn attribute_transfer_carries_values_from_nearby_points() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let brush = line(&mut b, 2);
    let paint = b.node(
        "pnt.attribute",
        &[
            ("name", json!("Cd")),
            ("type", json!("vec4")),
            ("source", json!("index")),
            ("value", json!([1.0, 0.0, 0.0, 1.0])),
            ("value2", json!([0.0, 0.0, 1.0, 1.0])),
        ],
    );
    let canvas = line(&mut b, 21);
    let hard = b.node(
        "pnt.attribute_transfer",
        &[("radius", json!(0.55)), ("fade", json!(false))],
    );
    let soft = b.node("pnt.attribute_transfer", &[("radius", json!(0.55))]);
    b.wire(brush, paint, "in");
    b.wire(canvas, hard, "in");
    b.wire(paint, hard, "from");
    b.wire(canvas, soft, "in");
    b.wire(paint, soft, "from");
    b.done();
    e.cook(&[hard, soft], 0.0).unwrap();
    let p = floats(&e, hard, "P");
    let hard = floats(&e, hard, "Cd");
    let soft = floats(&e, soft, "Cd");
    for i in 0..21 {
        let x = p[i][0];
        let (d, src) = if x < 0.0 {
            (x + 1.0, [1.0, 0.0, 0.0, 1.0])
        } else {
            (1.0 - x, [0.0, 0.0, 1.0, 1.0])
        };
        if d <= 0.55 - 1e-4 {
            assert_eq!(hard[i], src.to_vec(), "x {x}");
            let w = smoothstep(1.0 - d / 0.55);
            for c in 0..4 {
                let want = 1.0 + (src[c] - 1.0) * w;
                assert!((soft[i][c] - want).abs() < 1e-4, "x {x}: {:?}", soft[i]);
            }
        } else if d > 0.55 + 1e-4 {
            assert_eq!(hard[i], vec![1.0; 4], "out of reach: x {x}");
        }
    }
}

#[test]
fn lattice_moves_points_with_their_cage() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node("pnt.points", &[("count", json!(100)), ("seed", json!(4))]);
    // A 2 × 2 × 2 cage moved as a whole moves everything inside with it;
    // with no noise the cage is at rest and nothing moves.
    let still = b.node("pnt.lattice", &[("amplitude", json!(0.0))]);
    let cage = b.node(
        "pnt.lattice",
        &[
            ("output", json!("cage")),
            ("amplitude", json!(0.0)),
            ("divisions", json!([2.0, 2.0, 2.0])),
        ],
    );
    let shifted = b.node("pnt.transform", &[("translate", json!([0.25, -0.5, 0.0]))]);
    let moved = b.node("pnt.lattice", &[("divisions", json!([2.0, 2.0, 2.0]))]);
    let stretched_cage = b.node("pnt.transform", &[("scale", json!([2.0, 1.0, 1.0]))]);
    let stretched = b.node("pnt.lattice", &[("divisions", json!([2.0, 2.0, 2.0]))]);
    let noisy = b.node("pnt.lattice", &[]);
    b.wire(pts, still, "in");
    b.wire(cage, shifted, "in");
    b.wire(pts, moved, "in");
    b.wire(shifted, moved, "cage");
    b.wire(cage, stretched_cage, "in");
    b.wire(pts, stretched, "in");
    b.wire(stretched_cage, stretched, "cage");
    b.wire(pts, noisy, "in");
    b.done();
    e.cook(&[still, cage, moved, stretched, noisy], 0.0)
        .unwrap();
    let p = floats(&e, pts, "P");
    assert_eq!(e.geometry(cage).unwrap().points(), 8);
    assert_eq!(e.geometry(cage).unwrap().prims(), 12, "a box's edges");
    let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
    for (i, q) in floats(&e, still, "P").iter().enumerate() {
        assert!((0..3).all(|c| close(q[c], p[i][c])), "{q:?} vs {:?}", p[i]);
    }
    for (i, q) in floats(&e, moved, "P").iter().enumerate() {
        assert!(close(q[0], p[i][0] + 0.25) && close(q[1], p[i][1] - 0.5));
    }
    for (i, q) in floats(&e, stretched, "P").iter().enumerate() {
        assert!(
            close(q[0], p[i][0] * 2.0) && close(q[1], p[i][1]),
            "linear inside"
        );
    }
    let n = floats(&e, noisy, "P");
    assert!(n.iter().zip(&p).any(|(a, b)| (a[0] - b[0]).abs() > 1e-3));
}

#[test]
fn trails_keep_history_as_lines() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node(
        "pnt.points",
        &[("shape", json!("line")), ("count", json!(3))],
    );
    let mover = b.node("pnt.transform", &[]);
    let trail = b.node("pnt.trail", &[("length", json!(4))]);
    b.wire(pts, mover, "in");
    b.wire(mover, trail, "in");
    b.edits.push(Edit::SetParam {
        node: mover,
        param: "translate".into(),
        value: Some(Param::constant(json!([0.0, 0.0, 0.0]))),
    });
    b.done();
    // Move up by 0.1 a frame: frame f is at y = 0.1 f.
    for f in 0..6 {
        e.edit(
            "move",
            vec![Edit::SetParam {
                node: mover,
                param: "translate".into(),
                value: Some(Param::constant(json!([0.0, 0.1 * f as f64, 0.0]))),
            }],
        )
        .unwrap();
        e.cook(&[trail], f as f64 / 60.0).unwrap();
    }
    let g = e.geometry(trail).unwrap();
    assert_eq!(g.points(), 12);
    assert_eq!(g.prims(), 3, "one line per point");
    let p = floats(&e, trail, "P");
    let age = floats(&e, trail, "age");
    for src in 0..3 {
        for k in 0..4 {
            let j = src * 4 + k;
            let want = 0.1 * (5 - k) as f32;
            assert!(
                (p[j][1] - want).abs() < 1e-5,
                "point {src} step {k}: {}",
                p[j][1]
            );
            assert!((age[j][0] - k as f32 / 3.0).abs() < 1e-6);
        }
    }
}

#[test]
fn flocks_steer_toward_neighbours_and_hold_speed() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node(
        "pnt.scatter",
        &[
            ("shape", json!("ball")),
            ("count", json!(300)),
            ("radius", json!(0.5)),
        ],
    );
    let flock = b.node(
        "pnt.flock",
        &[("min_speed", json!(0.3)), ("max_speed", json!(0.8))],
    );
    let again = b.node(
        "pnt.flock",
        &[("min_speed", json!(0.3)), ("max_speed", json!(0.8))],
    );
    b.wire(pts, flock, "in");
    b.wire(pts, again, "in");
    b.done();
    e.cook(&[flock, again], 0.0).unwrap();
    let v = floats(&e, flock, "v");
    assert_eq!(v, floats(&e, again, "v"), "repeats exactly");
    for row in &v {
        let s = (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt();
        assert!((0.3 - 1e-4..=0.8 + 1e-4).contains(&s), "speed {s}");
    }
}

#[test]
fn attribute_promote_reduces_over_live_points() {
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let pts = b.node(
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(1000)),
            ("seed", json!(5)),
        ],
    );
    let ops = ["min", "max", "sum", "average"];
    let nodes: Vec<NodeId> = ops
        .iter()
        .map(|op| {
            let n = b.node("pnt.attribute_promote", &[("operation", json!(op))]);
            b.wire(pts, n, "in");
            n
        })
        .collect();
    // Every third point, counted on the GPU: only those are averaged.
    let some = b.node("pnt.subset", &[("nth", json!(3))]);
    let avg = b.node("pnt.attribute_promote", &[]);
    b.wire(pts, some, "in");
    b.wire(some, avg, "in");
    b.done();
    let mut all = nodes.clone();
    all.push(avg);
    e.cook(&all, 0.0).unwrap();
    let p = floats(&e, pts, "P");
    for (k, op) in ops.iter().enumerate() {
        let got = floats(&e, nodes[k], "promoted");
        for c in 0..3 {
            let col = p.iter().map(|r| r[c]);
            let want = match *op {
                "min" => col.fold(f32::MAX, f32::min),
                "max" => col.fold(f32::MIN, f32::max),
                "sum" => col.sum(),
                _ => col.sum::<f32>() / 1000.0,
            };
            let tol = if *op == "sum" { 1e-2 } else { 1e-5 };
            assert!(
                got.iter().all(|r| (r[c] - want).abs() < tol),
                "{op} {c}: {} vs {want}",
                got[0][c]
            );
        }
    }
    let live: Vec<&Vec<f32>> = p.iter().step_by(3).collect();
    let want = live.iter().map(|r| r[0]).sum::<f32>() / live.len() as f32;
    let got = floats(&e, avg, "promoted");
    assert!((got[0][0] - want).abs() < 1e-5, "{} vs {want}", got[0][0]);
}

#[test]
fn ray_intersect_projects_points_onto_polygons() {
    let Some(mut e) = engine() else { return };
    // MSH operators cook on worker threads: offline, each cook waits.
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let mut b = Build::new(&mut e);
    let floor = b.node(
        "msh.grid",
        &[("plane", json!("xz")), ("size", json!([4.0, 4.0]))],
    );
    let rain = b.node(
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(200)),
            ("centre", json!([0.0, 2.0, 0.0])),
        ],
    );
    let land = b.node("pnt.ray_intersect", &[]);
    let ball = b.node(
        "msh.sphere",
        &[
            ("type", json!("ico")),
            ("radius", json!(1.0)),
            ("level", json!(3)),
        ],
    );
    let shell = b.node(
        "pnt.scatter",
        &[
            ("shape", json!("sphere")),
            ("count", json!(200)),
            ("radius", json!(2.0)),
        ],
    );
    let wrap = b.node("pnt.ray_intersect", &[("direction_from", json!("point"))]);
    let miss = b.node(
        "pnt.ray_intersect",
        &[("direction", json!([0.0, 1.0, 0.0]))],
    );
    b.wire(rain, land, "in");
    b.wire(floor, land, "target");
    b.wire(shell, wrap, "in");
    b.wire(ball, wrap, "target");
    b.wire(rain, miss, "in");
    b.wire(floor, miss, "target");
    b.done();
    e.cook(&[land, wrap, miss], 0.0).unwrap();
    let before = floats(&e, rain, "P");
    let after = floats(&e, land, "P");
    let hd = floats(&e, land, "hitdist");
    for i in 0..200 {
        assert!(after[i][1].abs() < 1e-5, "on the floor: {:?}", after[i]);
        assert!((hd[i][0] - before[i][1]).abs() < 1e-5);
        assert!((after[i][0] - before[i][0]).abs() < 1e-6, "straight down");
    }
    for q in floats(&e, wrap, "P") {
        let r = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
        assert!((0.95..=1.0 + 1e-4).contains(&r), "on the ball: {r}");
    }
    let hd = floats(&e, miss, "hitdist");
    assert!(
        hd.iter().all(|h| h[0] == -1.0),
        "upward rays miss the floor"
    );
    assert_eq!(floats(&e, miss, "P"), before, "misses stay put");
}
