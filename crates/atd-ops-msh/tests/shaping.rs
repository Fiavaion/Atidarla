//! Shaping and repair operators on the CPU: Bevel, Remesh, Smooth, Clean,
//! Mirror, Clip and Measure, with expected counts, bounds, closedness,
//! volumes and determinism.

mod common;

use atd_core::geo::AttrClass;
use atd_ops_msh::Mesh;
use common::*;
use serde_json::json;

fn radii(m: &Mesh) -> (f64, f64) {
    m.p.iter()
        .map(|p| p.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>().sqrt())
        .fold((f64::MAX, f64::MIN), |(lo, hi), r| (lo.min(r), hi.max(r)))
}

/// A sphere of radius 1 with every point pushed in or out by up to 5%.
fn noisy_sphere() -> Mesh {
    let mut s = make(
        "msh.sphere",
        &[
            ("rows", json!(24)),
            ("columns", json!(48)),
            ("radius", json!(1.0)),
        ],
    );
    for (i, p) in s.p.iter_mut().enumerate() {
        let k = 1.0 + 0.05 * (((i * 7919) % 13) as f32 / 6.0 - 1.0);
        *p = p.map(|x| x * k);
    }
    s
}

fn spread(m: &Mesh) -> f64 {
    let (lo, hi) = radii(m);
    hi - lo
}

#[test]
fn smooth_removes_noise_and_taubin_keeps_the_size() {
    let s = noisy_sphere();
    let v0 = volume(&s);
    let lap = on(
        "msh.smooth",
        &[("method", json!("laplacian")), ("iterations", json!(20))],
        &s,
    );
    let tau = on("msh.smooth", &[("iterations", json!(20))], &s);
    assert_eq!((lap.points(), lap.prims()), (s.points(), s.prims()));
    assert_eq!(tau.corners, s.corners, "topology unchanged");
    assert!(
        spread(&tau) < spread(&s) * 0.5,
        "{} {}",
        spread(&tau),
        spread(&s)
    );
    assert!(spread(&lap) < spread(&s) * 0.5);
    let shrink_lap = 1.0 - volume(&lap) / v0;
    let shrink_tau = 1.0 - volume(&tau) / v0;
    assert!(shrink_lap > 0.05, "laplacian shrinks: {shrink_lap}");
    assert!(
        shrink_tau.abs() < shrink_lap / 4.0,
        "taubin keeps the size: {shrink_tau}"
    );
    assert!(unit_normals(&tau, 1e-4), "normals refreshed");
    // An open grid keeps its border; a bump in the middle flattens.
    let mut g = make("msh.grid", &[("rows", json!(9)), ("columns", json!(9))]);
    g.p[40][2] = 0.5;
    let sg = on("msh.smooth", &[("iterations", json!(30))], &g);
    assert_eq!(sg.bounds().unwrap().0[0], g.bounds().unwrap().0[0]);
    assert!(sg.p[40][2].abs() < 0.1, "{}", sg.p[40][2]);
    // A group limits what moves.
    let grouped = on(
        "msh.group",
        &[
            ("name", json!("left")),
            ("class", json!("point")),
            ("box_centre", json!([-0.6, 0.0, 0.0])),
            ("box_size", json!([0.8, 2.0, 2.0])),
        ],
        &g,
    );
    let sl = on("msh.smooth", &[("group", json!("left"))], &grouped);
    assert_eq!(sl.p[40], g.p[40], "the bump is outside the group");
    assert_eq!(
        on("msh.smooth", &[], &s),
        on("msh.smooth", &[], &s),
        "deterministic"
    );
}

#[test]
fn clean_repairs_duplicates_degenerates_and_winding() {
    let b = make("msh.box", &[]);
    let mut dirty = b.clone();
    // A duplicate of polygon 0 (reversed), a degenerate triangle, an
    // unused point, a flipped face and a duplicated point.
    let p0: Vec<u32> = dirty.prim(0).iter().rev().copied().collect();
    dirty.add_polygon(&p0);
    dirty.add_polygon(&[0, 1, 1]);
    dirty.add_point([9.0, 9.0, 9.0]);
    let (a, z) = (dirty.start[2] as usize, dirty.start[3] as usize);
    dirty.corners[a..z].reverse();
    let dup = dirty.add_point(dirty.p[0]);
    let first = dirty.prim(1).iter().position(|&c| c == 0);
    if let Some(i) = first {
        let at = dirty.start[1] as usize + i;
        dirty.corners[at] = dup;
    }
    dirty.point_attrs.clear();
    assert!(!watertight(&dirty));
    let c = on("msh.clean", &[], &dirty);
    assert_eq!((c.points(), c.prims()), (8, 6));
    assert!(watertight(&c));
    assert!(
        close(volume(&c), 1.0, 1e-6),
        "faces outward: {}",
        volume(&c)
    );
    // An inside-out box is turned outward.
    let mut inside_out = b.clone();
    inside_out
        .corners
        .chunks_exact_mut(4)
        .for_each(<[u32]>::reverse);
    let c = on("msh.clean", &[], &inside_out);
    assert!(close(volume(&c), 1.0, 1e-6));
    // Each repair can be turned off.
    let kept = on(
        "msh.clean",
        &[
            ("fuse", json!(false)),
            ("degenerate", json!(false)),
            ("duplicates", json!(false)),
            ("winding", json!(false)),
            ("unused", json!(false)),
        ],
        &dirty,
    );
    assert_eq!(kept, dirty);
}

#[test]
fn mirror_a_half_sphere_into_a_closed_one() {
    let s = make(
        "msh.sphere",
        &[
            ("rows", json!(16)),
            ("columns", json!(32)),
            ("radius", json!(1.0)),
        ],
    );
    let half = on("msh.clip", &[("normal", json!([1.0, 0.0, 0.0]))], &s);
    assert!(!watertight(&half));
    let whole = on("msh.mirror", &[], &half);
    assert!(watertight(&whole), "welded at the seam");
    assert!(
        close(volume(&whole), volume(&s), 1e-4),
        "{} {}",
        volume(&whole),
        volume(&s)
    );
    let mirrored = atd_ops_msh::edit::group_count(&whole, AttrClass::Prim, "mirrored");
    assert_eq!(mirrored * 2, whole.prims());
    // Without the original: the image only, on the other side, facing out.
    let image = on("msh.mirror", &[("keep_original", json!(false))], &half);
    assert_eq!(
        (image.points(), image.prims()),
        (half.points(), half.prims())
    );
    let (lo, hi) = image.bounds().unwrap();
    assert!(hi[0] <= 1e-6 && lo[0] < -0.9);
    let closed = on(
        "msh.clip",
        &[("cap", json!(true)), ("normal", json!([1.0, 0.0, 0.0]))],
        &s,
    );
    let flipped = on("msh.mirror", &[("keep_original", json!(false))], &closed);
    assert!(
        close(volume(&flipped), volume(&closed), 1e-9),
        "still outward"
    );
    // A box mirrored without merging: two separate closed boxes.
    let b = make("msh.box", &[("centre", json!([1.0, 0.0, 0.0]))]);
    let two = on("msh.mirror", &[("merge_seam", json!(false))], &b);
    assert_eq!((two.points(), two.prims()), (16, 12));
    assert!(watertight(&two));
    assert!(close(volume(&two), 2.0, 1e-6));
}

#[test]
fn clip_keeps_a_side_and_caps_the_cut() {
    let b = make("msh.box", &[]);
    let top = on("msh.clip", &[("origin", json!([0.0, 0.25, 0.0]))], &b);
    let (lo, hi) = top.bounds().unwrap();
    assert!(close(f64::from(lo[1]), 0.25, 1e-6) && close(f64::from(hi[1]), 0.5, 1e-6));
    assert_eq!(top.prims(), 5, "the top and four cut sides");
    assert!(!watertight(&top));
    let capped = on(
        "msh.clip",
        &[("origin", json!([0.0, 0.25, 0.0])), ("cap", json!(true))],
        &b,
    );
    assert!(watertight(&capped));
    assert!(close(volume(&capped), 0.25, 1e-6), "{}", volume(&capped));
    assert_eq!(
        atd_ops_msh::edit::group_count(&capped, AttrClass::Prim, "clip_cap"),
        1
    );
    // Both halves, as two closed pieces.
    let both = on(
        "msh.clip",
        &[
            ("keep", json!("both")),
            ("cap", json!(true)),
            ("normal", json!([1.0, 1.0, 0.0])),
        ],
        &b,
    );
    assert!(watertight(&both));
    assert!(close(volume(&both), 1.0, 1e-6), "{}", volume(&both));
    let above = atd_ops_msh::edit::group_count(&both, AttrClass::Prim, "above");
    let below = atd_ops_msh::edit::group_count(&both, AttrClass::Prim, "below");
    assert_eq!(above + below, both.prims());
    // A torus cut through its ring: the cap is a ring with a hole.
    let t = make(
        "msh.torus",
        &[
            ("rows", json!(16)),
            ("columns", json!(32)),
            ("radius", json!(0.5)),
            ("thickness", json!(0.2)),
        ],
    );
    let half = on("msh.clip", &[("cap", json!(true))], &t);
    assert!(watertight(&half), "the annulus cap closes it");
    assert!(
        close(volume(&half), volume(&t) / 2.0, 1e-3),
        "{} {}",
        volume(&half),
        volume(&t)
    );
    // Polylines are cut into runs.
    let line = make(
        "msh.line",
        &[
            ("start", json!([0.0, -1.0, 0.0])),
            ("end", json!([0.0, 1.0, 0.0])),
            ("points", json!(5)),
        ],
    );
    let cut = on("msh.clip", &[("origin", json!([0.0, 0.25, 0.0]))], &line);
    assert_eq!(cut.prims(), 1);
    assert!(!cut.closed[0]);
    assert_eq!(cut.points(), 3, "the cut point, 0.5 and 1");
    assert_eq!(
        on("msh.clip", &[("cap", json!(true))], &t),
        half,
        "deterministic"
    );
}

#[test]
fn measure_area_perimeter_and_curvature() {
    let g = make("msh.grid", &[("rows", json!(3)), ("columns", json!(3))]);
    let m = on(
        "msh.measure",
        &[("perimeter", json!(true)), ("normals", json!(true))],
        &g,
    );
    let a = m.prim_attr("area").unwrap().floats().unwrap();
    assert_eq!(a.len(), 4);
    assert!(a.iter().all(|&x| (x - 1.0).abs() < 1e-6), "{a:?}");
    let p = m.prim_attr("perimeter").unwrap().floats().unwrap();
    assert!(p.iter().all(|&x| (x - 4.0).abs() < 1e-6));
    let n = m.prim_attr("N").unwrap().floats().unwrap();
    assert_eq!(&n[..3], [0.0, 0.0, 1.0]);
    let k = m.point_attr("curvature").unwrap().floats().unwrap();
    assert!(k.iter().all(|&x| x.abs() < 1e-6), "a flat grid has none");
    let s = make(
        "msh.sphere",
        &[
            ("rows", json!(48)),
            ("columns", json!(96)),
            ("radius", json!(2.0)),
        ],
    );
    let median = |m: &Mesh| {
        let mut v = m
            .point_attr("curvature")
            .unwrap()
            .floats()
            .unwrap()
            .to_vec();
        v.sort_by(f32::total_cmp);
        f64::from(v[v.len() / 2])
    };
    let mean = on("msh.measure", &[], &s);
    assert!(close(median(&mean), 0.5, 0.02), "1/r: {}", median(&mean));
    let gauss = on("msh.measure", &[("curvature", json!("gaussian"))], &s);
    assert!(
        close(median(&gauss), 0.25, 0.03),
        "1/r²: {}",
        median(&gauss)
    );
    let total: f64 = on("msh.measure", &[("curvature", json!("none"))], &s)
        .prim_attr("area")
        .unwrap()
        .floats()
        .unwrap()
        .iter()
        .map(|&x| f64::from(x))
        .sum();
    assert!(close(total, 16.0 * std::f64::consts::PI, 0.01), "{total}");
    assert!(
        on("msh.measure", &[("curvature", json!("none"))], &s)
            .point_attr("curvature")
            .is_none()
    );
}

#[test]
fn bevel_chamfers_and_rounds_a_box() {
    let b = make("msh.box", &[]);
    let d = 0.1;
    let c = on("msh.bevel", &[("offset", json!(d))], &b);
    assert_eq!(
        c.prims(),
        6 + 12 + 8,
        "faces, edge strips, corner triangles"
    );
    assert_eq!(c.points(), 24);
    assert!(watertight(&c));
    let chamfer = 1.0 - 6.0 * d * d + 16.0 / 3.0 * d * d * d;
    assert!(close(volume(&c), chamfer, 1e-6), "{} {chamfer}", volume(&c));
    assert_eq!(c.bounds(), b.bounds(), "the faces stay where they were");
    assert_eq!(
        atd_ops_msh::edit::group_count(&c, AttrClass::Prim, "bevel"),
        20
    );
    assert!(unit_normals(&c, 1e-5));
    // Rounded: between the chamfer and a true quarter-round.
    let r = on(
        "msh.bevel",
        &[("offset", json!(d)), ("segments", json!(4))],
        &b,
    );
    assert!(watertight(&r));
    let pi = std::f64::consts::PI;
    let round = 1.0
        - 12.0 * (d * d - pi * d * d / 4.0) * (1.0 - 2.0 * d)
        - 8.0 * (d * d * d - pi * d * d * d / 6.0);
    let v = volume(&r);
    assert!(v > chamfer && v < round, "{chamfer} < {v} < {round}");
    assert_eq!(c.bounds(), r.bounds());
    // A divided box: strips continue through the edge midpoints.
    let b2 = make("msh.box", &[("divisions", json!(2))]);
    let c2 = on("msh.bevel", &[("offset", json!(d))], &b2);
    assert!(watertight(&c2));
    assert!(close(volume(&c2), chamfer, 1e-6), "{}", volume(&c2));
    assert_eq!(c2.prims(), 24 + 24 + 8);
    // A smooth sphere has no sharp edges: nothing changes.
    let s = make("msh.sphere", &[]);
    let same = on("msh.bevel", &[], &s);
    assert_eq!(
        (same.points(), same.prims(), &same.corners),
        (s.points(), s.prims(), &s.corners)
    );
    // Only the edges of a group: one bevelled edge along the top front.
    let g = on(
        "msh.group",
        &[
            ("name", json!("edge")),
            ("class", json!("point")),
            ("box_centre", json!([0.0, 0.5, 0.5])),
            ("box_size", json!([2.0, 0.1, 0.1])),
        ],
        &b,
    );
    let one = on(
        "msh.bevel",
        &[("offset", json!(d)), ("group", json!("edge"))],
        &g,
    );
    assert!(watertight(&one));
    assert!(
        close(volume(&one), 1.0 - d * d / 2.0, 1e-6),
        "{}",
        volume(&one)
    );
    assert_eq!(
        on("msh.bevel", &[("segments", json!(3))], &b),
        on("msh.bevel", &[("segments", json!(3))], &b)
    );
}

/// Mean edge length and the share of edges within `lo..hi`.
fn edge_stats(m: &Mesh, lo: f64, hi: f64) -> (f64, f64) {
    let mut lens = Vec::new();
    for f in 0..m.prims() {
        let p = m.prim(f);
        for i in 0..p.len() {
            let (a, b) = (m.p[p[i] as usize], m.p[p[(i + 1) % p.len()] as usize]);
            let d: f64 = (0..3)
                .map(|k| f64::from(a[k] - b[k]).powi(2))
                .sum::<f64>()
                .sqrt();
            lens.push(d);
        }
    }
    let mean = lens.iter().sum::<f64>() / lens.len() as f64;
    let within = lens.iter().filter(|&&x| x >= lo && x <= hi).count();
    (mean, within as f64 / lens.len() as f64)
}

#[test]
fn remesh_makes_even_triangles_on_the_same_surface() {
    let s = make(
        "msh.sphere",
        &[
            ("rows", json!(12)),
            ("columns", json!(24)),
            ("radius", json!(1.0)),
        ],
    );
    let l = 0.15;
    let r = on("msh.remesh", &[("edge_length", json!(l))], &s);
    assert!((0..r.prims()).all(|f| r.prim(f).len() == 3), "triangles");
    assert!(watertight(&r));
    let (mean, within) = edge_stats(&r, 0.6 * l, 1.4 * l);
    assert!(mean > 0.85 * l && mean < 1.15 * l, "mean edge {mean}");
    assert!(within > 0.95, "{within} of the edges near the length");
    let (_, input_within) = edge_stats(&s, 0.6 * l, 1.4 * l);
    assert!(input_within < 0.6, "the input was uneven: {input_within}");
    let (rlo, rhi) = radii(&r);
    assert!(
        rlo > 0.95 && rhi < 1.0 + 1e-4,
        "on the input surface: {rlo} {rhi}"
    );
    assert!(
        close(volume(&r), volume(&s), 0.01),
        "{} {}",
        volume(&r),
        volume(&s)
    );
    assert!(unit_normals(&r, 1e-5));
    assert_eq!(
        on("msh.remesh", &[("edge_length", json!(l))], &s),
        r,
        "deterministic"
    );
    // A box keeps its creases and an open grid its border.
    let b = make("msh.box", &[]);
    let rb = on("msh.remesh", &[("edge_length", json!(0.1))], &b);
    assert!(watertight(&rb));
    assert!(close(volume(&rb), 1.0, 1e-4), "{}", volume(&rb));
    assert_eq!(rb.bounds(), b.bounds());
    assert!(rb.prims() > 600, "{}", rb.prims());
    let g = make("msh.grid", &[("rows", json!(3)), ("columns", json!(3))]);
    let rg = on("msh.remesh", &[("edge_length", json!(0.2))], &g);
    assert!(close(area(&rg), 4.0, 1e-6), "{}", area(&rg));
    assert_eq!(rg.bounds(), g.bounds());
    assert_eq!(
        rg.point_attr("uv").unwrap().len(),
        rg.points(),
        "attributes follow"
    );
    let err = atd_ops_msh::run("msh.remesh", &[("edge_length", json!(1e-5))], &[Some(s)]);
    assert!(err.is_err_and(|e| e.contains("limit")));
}

#[test]
fn awkward_inputs_never_fail() {
    let b = make("msh.box", &[]);
    let mut fin = b.clone();
    // A fin on one box edge: three polygons share it.
    let tip = fin.add_point([0.0, 2.0, 2.0]);
    let (a, z) = (fin.prim(0)[0], fin.prim(0)[1]);
    fin.add_polygon(&[a, z, tip]);
    fin.point_attrs.clear();
    let mut points = Mesh::new();
    points.add_point([0.0, 0.0, 0.0]);
    points.add_point([1.0, 0.0, 0.0]);
    let mut sliver = Mesh::new();
    for p in [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]] {
        sliver.add_point(p);
    }
    sliver.add_polygon(&[0, 1, 2]);
    let inputs = [
        Mesh::new(),
        points,
        sliver,
        fin,
        make("msh.line", &[("points", json!(7))]),
        make("msh.grid", &[]),
        make("msh.circle", &[]),
        make("msh.tube", &[]),
        make("msh.sphere", &[("type", json!("ico")), ("level", json!(2))]),
        run("msh.merge", &[], &[Some(b.clone()), Some(b)]),
    ];
    let ops: [(&str, &[(&str, serde_json::Value)]); 10] = [
        ("msh.bevel", &[]),
        (
            "msh.bevel",
            &[("segments", json!(3)), ("angle", json!(0.0))],
        ),
        ("msh.remesh", &[("edge_length", json!(0.3))]),
        ("msh.smooth", &[("keep_boundary", json!(false))]),
        ("msh.clean", &[]),
        ("msh.mirror", &[]),
        (
            "msh.mirror",
            &[
                ("normal", json!([0.0, 1.0, 0.0])),
                ("origin", json!([0.0, 0.5, 0.0])),
            ],
        ),
        ("msh.clip", &[("cap", json!(true))]),
        (
            "msh.clip",
            &[
                ("keep", json!("both")),
                ("cap", json!(true)),
                ("normal", json!([0.3, 1.0, 0.2])),
            ],
        ),
        (
            "msh.measure",
            &[("perimeter", json!(true)), ("curvature", json!("gaussian"))],
        ),
    ];
    for m in &inputs {
        for (op, params) in &ops {
            atd_ops_msh::run(op, params, &[Some(m.clone())])
                .unwrap_or_else(|e| panic!("{op} on {} points: {e}", m.points()));
        }
    }
    for (op, _) in &ops {
        assert_eq!(run(op, &[], &[]), Mesh::new(), "{op} without an input");
    }
}
