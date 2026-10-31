//! M6 construction operators on the CPU: Revolve, Carve, Convex Hull,
//! Fracture, UV Unwrap and Text 3D, with expected counts, closedness,
//! volumes, areas and determinism.

mod common;

use atd_core::geo::AttrClass;
use atd_ops_msh::Mesh;
use common::*;
use serde_json::json;

/// A polyline through `pts`.
fn polyline(pts: &[[f32; 3]]) -> Mesh {
    let mut m = Mesh::new();
    let ids: Vec<u32> = pts.iter().map(|&p| m.add_point(p)).collect();
    m.add_prim(&ids, false);
    m
}

fn uv_area(m: &Mesh) -> f64 {
    let uv = m.point_attr("uv").unwrap().floats().unwrap();
    let mut a = 0.0;
    for f in 0..m.prims() {
        let p = m.prim(f);
        for i in 1..p.len() - 1 {
            let q = |k: u32| [uv[k as usize * 2] as f64, uv[k as usize * 2 + 1] as f64];
            let (x, y, z) = (q(p[0]), q(p[i]), q(p[i + 1]));
            a += ((y[0] - x[0]) * (z[1] - x[1]) - (y[1] - x[1]) * (z[0] - x[0])) / 2.0;
        }
    }
    a
}

#[test]
fn revolve_a_line_into_a_closed_cylinder_and_a_cone() {
    // A profile up the side and over both ends: on the axis at each end.
    let profile = polyline(&[
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 2.0, 0.0],
        [0.0, 2.0, 0.0],
    ]);
    let r = on("msh.revolve", &[("divisions", json!(64))], &profile);
    // Two axis points welded; 64 quads on the side, 64 triangles per cap.
    assert_eq!(r.points(), 2 + 2 * 64);
    assert_eq!(r.prims(), 3 * 64);
    assert!(watertight(&r), "welded on the axis, closed");
    let exact = std::f64::consts::PI * 2.0;
    let v = volume(&r);
    // A 64-gon prism: area 64/2 sin(2π/64) × height 2.
    let polygon = 32.0 * (std::f64::consts::TAU / 64.0).sin() * 2.0;
    assert!(close(v, polygon, 1e-5), "{v} vs {polygon} (circle {exact})");
    assert!(unit_normals(&r, 1e-4));
    assert_eq!(r.point_attr("uv").unwrap().len(), r.points());
    // A half turn of a closed square profile, capped: closed too.
    let mut sq = Mesh::new();
    for p in [
        [1.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
    ] {
        sq.add_point(p);
    }
    sq.add_polygon(&[0, 1, 2, 3]);
    let half = on(
        "msh.revolve",
        &[("end", json!(180.0)), ("divisions", json!(32))],
        &sq,
    );
    assert_eq!(half.prims(), 32 * 4 + 2);
    assert!(watertight(&half));
    // A half ring: π/2 × (r2² − r1²) × height.
    let ring = std::f64::consts::FRAC_PI_2 * 3.0;
    assert!(close(volume(&half).abs(), ring, 5e-3), "{}", volume(&half));
    // Reversing flips the sign of the volume.
    let rev = on(
        "msh.revolve",
        &[("divisions", json!(64)), ("reverse", json!(true))],
        &profile,
    );
    assert!(close(volume(&rev), -v, 1e-9));
    // A negative sweep still faces outward.
    let neg = on(
        "msh.revolve",
        &[("end", json!(-360.0)), ("divisions", json!(64))],
        &profile,
    );
    assert!(close(volume(&neg), v, 1e-5), "{}", volume(&neg));
}

#[test]
fn carve_keeps_a_share_of_a_curve_and_a_uv_band_of_a_surface() {
    let line = make(
        "msh.line",
        &[
            ("start", json!([0.0, 0.0, 0.0])),
            ("end", json!([4.0, 0.0, 0.0])),
            ("points", json!(5)),
        ],
    );
    let c = on(
        "msh.carve",
        &[("start", json!(0.1)), ("end", json!(0.6))],
        &line,
    );
    assert_eq!(c.prims(), 1);
    assert!(!c.closed[0]);
    let xs: Vec<f32> = c.prim(0).iter().map(|&i| c.p[i as usize][0]).collect();
    let want = [0.4, 1.0, 2.0, 2.4];
    assert_eq!(xs.len(), want.len());
    for (x, w) in xs.iter().zip(want) {
        assert!((x - w).abs() < 1e-5, "{xs:?}");
    }
    // Nothing kept when the range is empty.
    let none = on(
        "msh.carve",
        &[("start", json!(0.7)), ("end", json!(0.2))],
        &line,
    );
    assert_eq!(none.prims(), 0);
    // A 4 × 4 grid carved to the middle half in u: half the area, welded.
    let g = make("msh.grid", &[("rows", json!(5)), ("columns", json!(5))]);
    let full = area(&g);
    let band = on(
        "msh.carve",
        &[
            ("domain", json!("u")),
            ("start", json!(0.3)),
            ("end", json!(0.8)),
        ],
        &g,
    );
    assert!(close(area(&band), full * 0.5, 1e-5), "{}", area(&band));
    let uv = band.point_attr("uv").unwrap().floats().unwrap();
    assert!(
        uv.chunks_exact(2)
            .all(|q| q[0] >= 0.3 - 1e-6 && q[0] <= 0.8 + 1e-6),
        "uv interpolated into the band"
    );
    // Cuts are shared between neighbours: columns at u = 0.3 (cut), 0.5,
    // 0.75 and 0.8 (cut), 5 points each.
    assert_eq!(band.points(), 5 * 4);
    assert!(
        atd_ops_msh::run(
            "msh.carve",
            &[("domain", json!("u"))],
            &[Some(line.clone())]
        )
        .is_err(),
        "carving by u needs uv"
    );
}

#[test]
fn convex_hull_of_a_cloud_and_of_flat_points() {
    let s = make("msh.sphere", &[("type", json!("ico")), ("level", json!(2))]);
    let pts = on("msh.scatter", &[("count", json!(400))], &s);
    // Add the sphere's own points: they are all on the hull.
    let both = run("msh.merge", &[], &[Some(pts.clone()), Some(s.clone())]);
    let h = on("msh.convex_hull", &[], &both);
    assert!(watertight(&h));
    assert!(volume(&h) > 0.0, "faces outward");
    // Euler: a closed triangulated sphere has 2V − 4 faces.
    assert_eq!(h.prims(), 2 * h.points() - 4);
    // The icosphere is convex: its hull is itself (same volume).
    let hs = on("msh.convex_hull", &[], &s);
    assert!(close(volume(&hs), volume(&s), 1e-6));
    assert_eq!(hs.points(), s.points());
    // Every input point is inside or on the hull.
    for p in &both.p {
        for f in 0..h.prims() {
            let n = h.prim_normal(f);
            let a = h.p[h.prim(f)[0] as usize];
            let d: f64 = (0..3)
                .map(|k| n[k] * (f64::from(p[k]) - f64::from(a[k])))
                .sum();
            assert!(d <= 1e-6, "a point outside the hull: {d}");
        }
    }
    // A grid is flat: one convex polygon, its outline.
    let g = make("msh.grid", &[("rows", json!(4)), ("columns", json!(4))]);
    let flat = on("msh.convex_hull", &[], &g);
    assert_eq!(flat.prims(), 1);
    assert_eq!(flat.prim(0).len(), 4, "collinear border points left out");
    assert!(close(area(&flat), area(&g), 1e-6));
    // Deterministic.
    assert_eq!(on("msh.convex_hull", &[], &both), h);
}

#[test]
fn fracture_a_box_into_pieces_that_fill_it() {
    let b = make("msh.box", &[]);
    let f = on(
        "msh.fracture",
        &[("count", json!(8)), ("seed", json!(3))],
        &b,
    );
    let piece = f.prim_attr("piece").unwrap();
    let ids = match &piece.values {
        atd_ops_msh::Values::U32(v) => v.clone(),
        _ => panic!("piece is u32"),
    };
    let n = *ids.iter().max().unwrap() as usize + 1;
    assert_eq!(n, 8, "one piece per seed point");
    // The pieces fill the box: their volumes add up to its volume.
    let mut total = 0.0;
    for k in 0..n {
        let keep: Vec<bool> = ids.iter().map(|&i| i as usize == k).collect();
        let mut m = f.clone();
        atd_ops_msh::edit::filter_prims(&mut m, &keep);
        m.remove_unused_points();
        let v = volume(&m);
        assert!(v > 0.0, "piece {k} is closed and outward: {v}");
        total += v;
    }
    assert!(close(total, 1.0, 1e-4), "{total}");
    let inside = atd_ops_msh::edit::group_count(&f, AttrClass::Prim, "inside");
    let outside = atd_ops_msh::edit::group_count(&f, AttrClass::Prim, "outside");
    assert!(inside > 0 && outside > 0 && inside + outside == f.prims());
    // The outside faces still cover the box's surface.
    let mut out = f.clone();
    let keep = f.group_members(AttrClass::Prim, "outside").unwrap();
    atd_ops_msh::edit::filter_prims(&mut out, &keep);
    assert!(close(area(&out), 6.0, 1e-4), "{}", area(&out));
    // A gap shrinks every piece.
    let g = on(
        "msh.fracture",
        &[
            ("count", json!(8)),
            ("seed", json!(3)),
            ("gap", json!(0.05)),
        ],
        &b,
    );
    assert!(volume(&g) < total - 0.01);
    // Seeds from the `points` input; the same seed gives the same pieces.
    let mut seeds = Mesh::new();
    seeds.add_point([-0.25, 0.0, 0.0]);
    seeds.add_point([0.25, 0.0, 0.0]);
    let two = run("msh.fracture", &[], &[Some(b.clone()), Some(seeds)]);
    let halves = atd_ops_msh::edit::group_count(&two, AttrClass::Prim, "inside");
    assert!(halves >= 2, "the cut face on each side");
    assert!(close(volume(&two), 1.0, 1e-6));
    assert_eq!(
        on(
            "msh.fracture",
            &[("count", json!(8)), ("seed", json!(3))],
            &b
        ),
        f
    );
}

#[test]
fn uv_unwrap_lays_out_charts_without_overlap_or_flips() {
    for method in ["conformal", "planar"] {
        let s = make("msh.sphere", &[("rows", json!(12)), ("columns", json!(24))]);
        let u = on("msh.uv_unwrap", &[("method", json!(method))], &s);
        assert_eq!(u.prims(), s.prims());
        let uv = u.point_attr("uv").unwrap().floats().unwrap();
        assert!(
            uv.iter().all(|&x| (-1e-6..=1.0 + 1e-6).contains(&x)),
            "{method}: inside the unit square"
        );
        // Every polygon keeps a positive uv area (no flips).
        for f in 0..u.prims() {
            let p = u.prim(f);
            let q = |k: u32| [uv[k as usize * 2] as f64, uv[k as usize * 2 + 1] as f64];
            let mut a = 0.0;
            for i in 1..p.len() - 1 {
                let (x, y, z) = (q(p[0]), q(p[i]), q(p[i + 1]));
                a += (y[0] - x[0]) * (z[1] - x[1]) - (y[1] - x[1]) * (z[0] - x[0]);
            }
            assert!(a >= -1e-12, "{method}: polygon {f} flipped");
        }
        // Charts do not overlap: their areas add up to at most the square.
        let a = uv_area(&u);
        assert!(a > 0.2 && a <= 1.0 + 1e-6, "{method}: {a}");
        // Same positions as the input, split along seams.
        assert!(u.points() >= s.points());
        assert!(close(area(&u), area(&s), 1e-6));
        let charts = u.prim_attr("chart").unwrap().len();
        assert_eq!(charts, u.prims());
    }
    // A flat grid is one chart, mapped without distortion: uv area ratio
    // per polygon is the same everywhere.
    let g = make("msh.grid", &[("rows", json!(6)), ("columns", json!(9))]);
    let u = on("msh.uv_unwrap", &[], &g);
    assert_eq!(u.points(), g.points(), "one chart, no seams");
    let uv = u.point_attr("uv").unwrap().floats().unwrap();
    let quad = |f: usize| {
        let p = u.prim(f);
        let q = |k: u32| [uv[k as usize * 2] as f64, uv[k as usize * 2 + 1] as f64];
        let (a, b, c) = (q(p[0]), q(p[1]), q(p[2]));
        ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs()
    };
    let first = quad(0);
    for f in 1..u.prims() {
        assert!(close(quad(f), first, 1e-4), "even texel density");
    }
    // A box: every face its own chart (90° apart).
    let b = make("msh.box", &[]);
    let ub = on("msh.uv_unwrap", &[], &b);
    assert_eq!(ub.points(), 24);
    assert!(close(uv_area(&ub), 6.0 * quad_area_of_box(&ub), 1e-6));
}

fn quad_area_of_box(m: &Mesh) -> f64 {
    let uv = m.point_attr("uv").unwrap().floats().unwrap();
    let p = m.prim(0);
    let q = |k: u32| [uv[k as usize * 2] as f64, uv[k as usize * 2 + 1] as f64];
    let (a, b, c) = (q(p[0]), q(p[1]), q(p[2]));
    ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs()
}

#[test]
fn text_3d_makes_closed_letters_with_holes() {
    let o = make(
        "msh.text_3d",
        &[("text", json!("O")), ("depth", json!(0.2))],
    );
    assert!(watertight(&o), "a closed solid");
    assert!(volume(&o) > 0.0, "faces outward");
    let front = atd_ops_msh::edit::group_count(&o, AttrClass::Prim, "front");
    let back = atd_ops_msh::edit::group_count(&o, AttrClass::Prim, "back");
    assert_eq!(front, back);
    // Volume = front area × depth (the hole is not filled).
    let mut f = o.clone();
    let keep = o.group_members(AttrClass::Prim, "front").unwrap();
    atd_ops_msh::edit::filter_prims(&mut f, &keep);
    let fa = area(&f);
    assert!(
        close(volume(&o), fa * 0.2, 1e-4),
        "{} vs {}",
        volume(&o),
        fa * 0.2
    );
    // An "O" with its hole: less area than its outer outline alone; a
    // solid bar of the same height has no hole.
    let bounds = o.bounds().unwrap();
    let outline = (bounds.1[0] - bounds.0[0]) * (bounds.1[1] - bounds.0[1]);
    assert!(
        fa < 0.6 * f64::from(outline),
        "the hole is open: {fa} of {outline}"
    );
    // Flat text: front faces only, all at z = 0.
    let flat = make(
        "msh.text_3d",
        &[("text", json!("Ab8\nxy")), ("depth", json!(0.0))],
    );
    assert!(flat.prims() > 0);
    assert!(flat.p.iter().all(|p| p[2] == 0.0));
    for f in 0..flat.prims() {
        let n = flat.prim_normal(f);
        assert!(n[2] >= 0.0, "facing +Z (or flat in f32): {n:?}");
    }
    // Centred on the origin by default; mono font works; empty is empty.
    let (lo, hi) = make("msh.text_3d", &[("font", json!("mono"))])
        .bounds()
        .unwrap();
    assert!(((lo[0] + hi[0]) / 2.0).abs() < 0.05 && ((lo[1] + hi[1]) / 2.0).abs() < 0.3);
    assert_eq!(make("msh.text_3d", &[("text", json!(""))]).prims(), 0);
    // Every letter of the alphabet closes up.
    let all = make(
        "msh.text_3d",
        &[(
            "text",
            json!("ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789&@%"),
        )],
    );
    assert!(watertight(&all));
    assert!(volume(&all) > 0.0);
}

/// Worker-side cook times (not a pass/fail test):
/// `cargo test -p atd-ops-msh --release --test construction -- --ignored --nocapture`.
#[test]
#[ignore = "timings: run with --release --ignored --nocapture"]
fn worker_cook_times() {
    let time =
        |label: &str, ty: &str, params: &[(&str, serde_json::Value)], inputs: &[Option<Mesh>]| {
            let mut best = f64::MAX;
            let mut out = Mesh::new();
            for _ in 0..3 {
                let t = std::time::Instant::now();
                out = run(ty, params, inputs);
                best = best.min(t.elapsed().as_secs_f64() * 1e3);
            }
            eprintln!(
                "{label:<40} {best:>9.2} ms  ({} points, {} prims)",
                out.points(),
                out.prims()
            );
        };
    let sphere = make(
        "msh.sphere",
        &[("rows", json!(128)), ("columns", json!(256))],
    );
    let ico = make("msh.sphere", &[("type", json!("ico")), ("level", json!(3))]);
    let cloud = on("msh.scatter", &[("count", json!(100_000))], &sphere);
    time(
        "convex hull of 100k points",
        "msh.convex_hull",
        &[],
        &[Some(cloud)],
    );
    time(
        "uv unwrap sphere 128 x 256",
        "msh.uv_unwrap",
        &[],
        &[Some(sphere.clone())],
    );
    time(
        "fracture ico level 3 into 20",
        "msh.fracture",
        &[("count", json!(20))],
        &[Some(ico)],
    );
    time(
        "text 3d, 40 characters",
        "msh.text_3d",
        &[("text", json!("The quick brown fox jumps over the lazy"))],
        &[],
    );
    let line = make(
        "msh.line",
        &[("points", json!(1000)), ("start", json!([0.5, 0.0, 0.0]))],
    );
    time(
        "revolve 1000 points x 256",
        "msh.revolve",
        &[("divisions", json!(256))],
        &[Some(line)],
    );
    time(
        "carve 512 x 512 grid to a uv band",
        "msh.carve",
        &[
            ("domain", json!("u")),
            ("start", json!(0.2)),
            ("end", json!(0.7)),
        ],
        &[Some(make(
            "msh.grid",
            &[("rows", json!(512)), ("columns", json!(512))],
        ))],
    );
}
