//! Modelling operators on the CPU: Subdivide, Extrude, UV Project,
//! PolyReduce, Boolean, Sweep, Skin, Scatter and Iso Surface, with
//! expected counts, closedness, volumes and determinism.

mod common;

use common::*;
use lsr_core::geo::AttrClass;
use lsr_ops_msh::Mesh;
use serde_json::json;

#[test]
fn subdivide_a_box_towards_a_rounded_shape() {
    let b = make("msh.box", &[]);
    let s1 = on("msh.subdivide", &[], &b);
    assert_eq!(s1.prims(), 24);
    assert_eq!(s1.points(), 8 + 6 + 12);
    assert!(watertight(&s1));
    let s3 = on("msh.subdivide", &[("levels", json!(3))], &b);
    assert_eq!(s3.prims(), 6 * 64);
    assert!(watertight(&s3));
    // Catmull-Clark shrinks a cube towards its limit surface, inside the
    // inscribed sphere (0.52): its corners move from 0.5 to 0.28 at once.
    let v = volume(&s3);
    assert!(v > 0.25 && v < 0.52, "{v}");
    assert!(unit_normals(&s3, 1e-5));
    // An open grid keeps its border on a curve, nothing breaks.
    let g = make("msh.grid", &[("rows", json!(3)), ("columns", json!(3))]);
    let sg = on("msh.subdivide", &[("levels", json!(2))], &g);
    assert_eq!(sg.prims(), 4 * 16);
    let uv = sg.point_attr("uv").unwrap();
    assert_eq!(uv.len(), sg.points(), "attributes interpolated");
    let err = lsr_ops_msh::run(
        "msh.subdivide",
        &[("levels", json!(6))],
        &[Some(make(
            "msh.sphere",
            &[("rows", json!(512)), ("columns", json!(1024))],
        ))],
    );
    assert!(
        err.is_err_and(|e| e.contains("limit")),
        "too large is refused before allocating"
    );
}

#[test]
fn extrude_individual_and_connected() {
    let b = make("msh.box", &[]);
    let e = on("msh.extrude", &[("distance", json!(0.5))], &b);
    // Each of 6 faces: a moved face and 4 walls.
    assert_eq!(e.prims(), 6 * 5);
    assert!(watertight(&e));
    // A cube with a 0.5-deep prism on every face: 1 + 6 × 0.5.
    assert!(close(volume(&e), 4.0, 1e-6), "{}", volume(&e));
    assert_eq!(
        lsr_ops_msh::edit::group_count(&e, AttrClass::Prim, "extrude_front"),
        6
    );
    assert_eq!(
        lsr_ops_msh::edit::group_count(&e, AttrClass::Prim, "extrude_side"),
        24
    );
    let inset = on(
        "msh.extrude",
        &[("distance", json!(0.0)), ("inset", json!(0.5))],
        &b,
    );
    assert!(watertight(&inset));
    assert!(close(volume(&inset), 1.0, 1e-6));
    // Connected: the top 2×2 faces of a divided box move as one block.
    let b2 = make("msh.box", &[("divisions", json!(2))]);
    let top = on(
        "msh.group",
        &[
            ("name", json!("top")),
            ("method", json!("normal")),
            ("angle", json!(10.0)),
        ],
        &b2,
    );
    let c = on(
        "msh.extrude",
        &[
            ("group", json!("top")),
            ("mode", json!("connected")),
            ("distance", json!(1.0)),
        ],
        &top,
    );
    assert_eq!(c.prims(), 24 + 8, "4 moved faces and 8 outline walls");
    assert!(watertight(&c));
    assert!(close(volume(&c), 2.0, 1e-6), "{}", volume(&c));
}

#[test]
fn uv_projections_cover_zero_to_one() {
    let s = make("msh.sphere", &[]);
    for proj in ["planar", "cylindrical", "spherical", "box"] {
        let m = on(
            "msh.uv_project",
            &[("projection", json!(proj)), ("axis", json!("y"))],
            &s,
        );
        let uv = m.point_attr("uv").unwrap().floats().unwrap();
        assert_eq!(uv.len(), m.points() * 2);
        let (lo, hi) = uv
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        assert!(lo >= -1e-6 && hi <= 1.0 + 1e-6, "{proj}: {lo} {hi}");
        assert!(hi - lo > 0.5, "{proj} spreads");
    }
    let g = make("msh.grid", &[]);
    let m = on(
        "msh.uv_project",
        &[("scale", json!([2.0, 2.0])), ("offset", json!([1.0, 0.0]))],
        &g,
    );
    let uv = m.point_attr("uv").unwrap().floats().unwrap();
    assert_eq!(&uv[..2], [1.0, 0.0]);
    assert_eq!(&uv[uv.len() - 2..], [3.0, 2.0]);
}

#[test]
fn polyreduce_keeps_the_shape() {
    let s = make(
        "msh.sphere",
        &[
            ("rows", json!(32)),
            ("columns", json!(64)),
            ("radius", json!(1.0)),
        ],
    );
    let tris = on("msh.triangulate", &[], &s).prims();
    let r = on("msh.polyreduce", &[("percent", json!(10.0))], &s);
    assert!(r.prims() <= tris / 10 + 2, "{} of {tris}", r.prims());
    assert!(r.prims() >= tris / 10 - 2, "{} of {tris}", r.prims());
    assert!(watertight(&r), "collapses keep a closed manifold");
    let exact = 4.0 / 3.0 * std::f64::consts::PI;
    assert!(close(volume(&r), exact, 0.05), "{}", volume(&r));
    // A tolerance stops early on a flat grid: everything there is free.
    let g = make("msh.grid", &[("rows", json!(10)), ("columns", json!(10))]);
    let rg = on(
        "msh.polyreduce",
        &[("percent", json!(0.0)), ("keep_boundary", json!(true))],
        &g,
    );
    assert!(rg.prims() < 81 * 2 / 4, "{}", rg.prims());
    assert!(
        close(area(&rg), 4.0, 1e-6),
        "a flat grid keeps its area: {}",
        area(&rg)
    );
    let bumpy = on(
        "msh.polyreduce",
        &[("percent", json!(0.0)), ("tolerance", json!(1e-4))],
        &s,
    );
    assert!(bumpy.prims() > r.prims(), "a tight tolerance keeps more");
    assert_eq!(
        on("msh.polyreduce", &[("percent", json!(10.0))], &s),
        r,
        "deterministic"
    );
}

#[test]
fn boolean_volumes_of_two_boxes() {
    let a = make("msh.box", &[]);
    let b = make("msh.box", &[("centre", json!([0.5, 0.5, 0.5]))]);
    let run2 = |op: &str| {
        run(
            "msh.boolean",
            &[("operation", json!(op))],
            &[Some(a.clone()), Some(b.clone())],
        )
    };
    let u = run2("union");
    let i = run2("intersect");
    let d = run2("subtract");
    assert!(close(volume(&u), 2.0 - 0.125, 1e-6), "union {}", volume(&u));
    assert!(close(volume(&i), 0.125, 1e-6), "intersect {}", volume(&i));
    assert!(
        close(volume(&d), 1.0 - 0.125, 1e-6),
        "subtract {}",
        volume(&d)
    );
    assert!(close(area(&i), 6.0 * 0.25, 1e-6), "{}", area(&i));
    let from_a = lsr_ops_msh::edit::group_count(&d, AttrClass::Prim, "from_a");
    let from_b = lsr_ops_msh::edit::group_count(&d, AttrClass::Prim, "from_b");
    assert!(from_a > 0 && from_b > 0 && from_a + from_b == d.prims());
    assert_eq!(run2("subtract"), d, "deterministic");
    // Sphere minus box: a closed-looking shape with the expected volume.
    let s = make("msh.sphere", &[("type", json!("ico")), ("level", json!(3))]);
    let cut = run(
        "msh.boolean",
        &[("operation", json!("subtract"))],
        &[Some(s.clone()), Some(b.clone())],
    );
    let half = volume(&s) * 7.0 / 8.0;
    assert!(
        close(volume(&cut), half, 0.02),
        "{} vs {half}",
        volume(&cut)
    );
    // Missing inputs pass A through.
    assert_eq!(run("msh.boolean", &[], &[Some(a.clone()), None]), a);
}

#[test]
fn sweep_a_circle_along_a_line_and_an_arc() {
    let line = make(
        "msh.line",
        &[("points", json!(5)), ("end", json!([0.0, 2.0, 0.0]))],
    );
    let tube = run(
        "msh.sweep",
        &[("radius", json!(0.25)), ("divisions", json!(64))],
        &[Some(line.clone()), None],
    );
    assert_eq!(tube.prims(), 4 * 64 + 2);
    assert!(watertight(&tube));
    let exact = std::f64::consts::PI * 0.0625 * 2.0;
    assert!(close(volume(&tube), exact, 0.01), "{}", volume(&tube));
    // A profile input and a closed curve: a torus-like ring, no caps.
    let ring = make(
        "msh.circle",
        &[
            ("type", json!("polyline")),
            ("plane", json!("xz")),
            ("divisions", json!(64)),
        ],
    );
    let square = make(
        "msh.circle",
        &[("divisions", json!(4)), ("radius", json!(0.1))],
    );
    let swept = run("msh.sweep", &[], &[Some(ring), Some(square)]);
    assert_eq!(swept.prims(), 64 * 4);
    assert!(
        watertight(&swept),
        "frames meet themselves on a closed curve"
    );
    // Scale to a point at the end: a cone.
    let cone = run(
        "msh.sweep",
        &[("scale_end", json!(0.0)), ("caps", json!(false))],
        &[Some(line), None],
    );
    let last = cone.p[cone.points() - 1];
    assert!((last[0]).abs() < 1e-6 && (last[2]).abs() < 1e-6);
}

#[test]
fn skin_lofts_profiles() {
    let circle = |r: f64, y: f64| {
        let c = make(
            "msh.circle",
            &[
                ("plane", json!("xz")),
                ("radius", json!(r)),
                ("divisions", json!(32)),
            ],
        );
        on("msh.transform", &[("translate", json!([0.0, y, 0.0]))], &c)
    };
    let profiles = run(
        "msh.merge",
        &[],
        &[
            Some(circle(0.5, 0.0)),
            Some(circle(0.3, 1.0)),
            Some(circle(0.5, 2.0)),
        ],
    );
    let s = on("msh.skin", &[], &profiles);
    assert_eq!(s.prims(), 2 * 32 + 2);
    assert!(watertight(&s));
    assert!(volume(&s) > 0.0, "faces outward");
    let lines = make("msh.line", &[]);
    let mut bad = run("msh.merge", &[], &[Some(lines), Some(circle(0.5, 1.0))]);
    let err = lsr_ops_msh::run("msh.skin", &[], &[Some(bad.clone())]).unwrap_err();
    assert!(err.contains("equal counts"), "{err}");
    bad.closed.clear();
    bad.start.truncate(1);
    bad.corners.clear();
    assert!(lsr_ops_msh::run("msh.skin", &[], &[Some(bad)]).is_err());
}

#[test]
fn scatter_is_seeded_area_weighted_and_on_the_surface() {
    let s = make("msh.sphere", &[("radius", json!(1.0))]);
    let p = on(
        "msh.scatter",
        &[("count", json!(2000)), ("seed", json!(7))],
        &s,
    );
    assert_eq!(p.points(), 2000);
    assert_eq!(p.prims(), 0);
    assert!(unit_normals(&p, 1e-5));
    for q in &p.p {
        let r = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
        assert!(r <= 1.0 + 1e-5 && r > 0.98, "{r}");
    }
    assert_eq!(
        on(
            "msh.scatter",
            &[("count", json!(2000)), ("seed", json!(7))],
            &s
        ),
        p
    );
    assert_ne!(
        on(
            "msh.scatter",
            &[("count", json!(2000)), ("seed", json!(8))],
            &s
        )
        .p,
        p.p
    );
    // Area weighting: a 1×1 and a 1×3 grid get about 1:3 of the points.
    let small = make(
        "msh.grid",
        &[
            ("size", json!([1.0, 1.0])),
            ("rows", json!(2)),
            ("columns", json!(2)),
        ],
    );
    let big = on(
        "msh.transform",
        &[
            ("translate", json!([5.0, 0.0, 0.0])),
            ("scale", json!([1.0, 3.0, 1.0])),
        ],
        &small,
    );
    let both = run("msh.merge", &[], &[Some(small), Some(big)]);
    let pts = on("msh.scatter", &[("count", json!(8000))], &both);
    let right = pts.p.iter().filter(|q| q[0] > 2.5).count();
    assert!(
        (5700..6300).contains(&right),
        "{right} of 8000 on the larger grid"
    );
    let ids = pts.point_attr("id").unwrap();
    assert_eq!(ids.len(), 8000);
}

#[test]
fn iso_surfaces_are_closed() {
    let sphere = make(
        "msh.iso_surface",
        &[
            ("source", json!("sphere")),
            ("radius", json!(1.0)),
            ("resolution", json!(32)),
        ],
    );
    assert!(watertight(&sphere));
    let exact = 4.0 / 3.0 * std::f64::consts::PI;
    assert!(close(volume(&sphere), exact, 0.05), "{}", volume(&sphere));
    assert!(unit_normals(&sphere, 1e-4));
    for shape in ["box", "torus", "gyroid"] {
        let m = make(
            "msh.iso_surface",
            &[
                ("source", json!(shape)),
                ("resolution", json!(24)),
                ("radius", json!(0.6)),
            ],
        );
        assert!(m.prims() > 50, "{shape}");
        assert!(watertight(&m), "{shape}");
        assert!(volume(&m) > 0.0, "{shape} faces outward");
    }
    let bx = make(
        "msh.iso_surface",
        &[
            ("source", json!("box")),
            ("size", json!([1.0, 1.0, 1.0])),
            ("resolution", json!(40)),
        ],
    );
    assert!(close(volume(&bx), 1.0, 0.05), "{}", volume(&bx));
    // From points: two touching balls blend into one closed blob.
    let mut pts = Mesh::new();
    pts.add_point([0.0, 0.0, 0.0]);
    pts.add_point([0.3, 0.0, 0.0]);
    let blob = on(
        "msh.iso_surface",
        &[
            ("radius", json!(0.2)),
            ("blend", json!(0.1)),
            ("resolution", json!(32)),
        ],
        &pts,
    );
    assert!(watertight(&blob));
    let v = volume(&blob);
    let one = 4.0 / 3.0 * std::f64::consts::PI * 0.008;
    assert!(v > one * 1.5 && v < one * 2.6, "{v} vs one ball {one}");
    // The PNT↔MSH chain on the CPU: scatter → iso surface.
    let s = make("msh.sphere", &[]);
    let p = on(
        "msh.scatter",
        &[("count", json!(300)), ("pscale", json!(1.0))],
        &s,
    );
    let iso = on(
        "msh.iso_surface",
        &[("radius", json!(0.08)), ("resolution", json!(48))],
        &p,
    );
    assert!(watertight(&iso));
    assert_eq!(
        on(
            "msh.iso_surface",
            &[("radius", json!(0.08)), ("resolution", json!(48))],
            &p
        ),
        iso
    );
}

#[test]
fn iso_surface_refuses_work_past_its_cap() {
    // 3000 points whose spheres each span the whole grid: ~800 million
    // node visits, refused up front with what to lower.
    let s = make("msh.sphere", &[]);
    let p = on(
        "msh.scatter",
        &[("count", json!(3000)), ("pscale", json!(1.0))],
        &s,
    );
    let t = std::time::Instant::now();
    let err = lsr_ops_msh::run(
        "msh.iso_surface",
        &[("radius", json!(50.0)), ("resolution", json!(64))],
        &[Some(p)],
    )
    .expect_err("past the cap");
    assert!(t.elapsed().as_secs_f64() < 1.0, "refused quickly");
    assert!(
        err.contains("Radius") && err.contains("Resolution") && err.contains("limit"),
        "{err}"
    );
}

#[test]
fn colour_modes() {
    let b = make("msh.box", &[]);
    let c = on(
        "msh.colour",
        &[("colour", json!([0.5, 0.25, 1.0, 1.0]))],
        &b,
    );
    let cd = c.point_attr("Cd").unwrap().floats().unwrap();
    assert_eq!(&cd[..4], [0.5, 0.25, 1.0, 1.0]);
    let n = on("msh.colour", &[("mode", json!("normal"))], &b);
    let cd = n.point_attr("Cd").unwrap().floats().unwrap();
    assert!(cd.iter().all(|&x| (0.0..=1.0).contains(&x)));
    let pos = on("msh.colour", &[("mode", json!("position"))], &b);
    let cd = pos.point_attr("Cd").unwrap().floats().unwrap();
    assert!(
        cd.chunks_exact(4)
            .all(|c| c[..3].iter().all(|&x| x == 0.0 || x == 1.0))
    );
    let per_prim = on("msh.colour", &[("mode", json!("random_prim"))], &b);
    assert_eq!(per_prim.points(), 24, "each face gets its own points");
    let cd = per_prim.point_attr("Cd").unwrap().floats().unwrap();
    for f in 0..per_prim.prims() {
        let p = per_prim.prim(f);
        let first = &cd[p[0] as usize * 4..p[0] as usize * 4 + 4];
        assert!(
            p.iter()
                .all(|&i| &cd[i as usize * 4..i as usize * 4 + 4] == first)
        );
    }
}
