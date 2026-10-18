//! Primitives and topology edits on the CPU: counts, bounds, closedness,
//! outward orientation, volumes and unit normals.

mod common;

use atd_core::geo::AttrClass;
use common::*;
use serde_json::json;

#[test]
fn grid_counts_uv_and_normals() {
    let g = make(
        "msh.grid",
        &[
            ("rows", json!(3)),
            ("columns", json!(4)),
            ("size", json!([3.0, 2.0])),
        ],
    );
    assert_eq!((g.points(), g.prims()), (12, 6));
    assert_eq!(g.bounds().unwrap(), ([-1.5, -1.0, 0.0], [1.5, 1.0, 0.0]));
    assert!(close(area(&g), 6.0, 1e-6));
    let uv = g.point_attr("uv").unwrap().floats().unwrap();
    assert_eq!(&uv[..2], [0.0, 0.0]);
    assert_eq!(&uv[uv.len() - 2..], [1.0, 1.0]);
    // Polygons face the plane's normal.
    assert!(g.prim_normal(0)[2] > 0.0);
    let xz = make("msh.grid", &[("plane", json!("xz"))]);
    assert!(xz.prim_normal(0)[1] > 0.0);
    assert!(unit_normals(&g, 1e-6));
}

#[test]
fn box_is_closed_with_the_right_volume() {
    for d in [1, 3] {
        let b = make(
            "msh.box",
            &[("size", json!([2.0, 3.0, 4.0])), ("divisions", json!(d))],
        );
        assert_eq!(b.prims(), 6 * d * d);
        assert_eq!(
            b.points(),
            6 * d * d + 2,
            "Euler: V = F + 2 for quads on a sphere"
        );
        assert!(watertight(&b), "divisions {d}");
        assert!(close(volume(&b), 24.0, 1e-6), "{}", volume(&b));
        assert!(unit_normals(&b, 1e-5));
    }
}

#[test]
fn spheres_are_closed_and_approach_the_volume() {
    let exact = 4.0 / 3.0 * std::f64::consts::PI;
    let polar = make(
        "msh.sphere",
        &[
            ("radius", json!(1.0)),
            ("rows", json!(64)),
            ("columns", json!(128)),
        ],
    );
    assert!(watertight(&polar));
    assert!(close(volume(&polar), exact, 0.01), "{}", volume(&polar));
    assert!(unit_normals(&polar, 1e-5));
    let ico = make(
        "msh.sphere",
        &[
            ("type", json!("ico")),
            ("radius", json!(1.0)),
            ("level", json!(4)),
        ],
    );
    assert_eq!(ico.prims(), 20 * 256);
    assert_eq!(ico.points(), 10 * 256 + 2);
    assert!(watertight(&ico));
    assert!(close(volume(&ico), exact, 0.01), "{}", volume(&ico));
}

#[test]
fn tubes_and_cones_are_closed() {
    let cyl = make("msh.tube", &[("columns", json!(256)), ("rows", json!(3))]);
    assert!(watertight(&cyl));
    let exact = std::f64::consts::PI * 0.25;
    assert!(close(volume(&cyl), exact, 0.001), "{}", volume(&cyl));
    let cone = make(
        "msh.tube",
        &[("radius_top", json!(0.0)), ("columns", json!(256))],
    );
    assert!(watertight(&cone));
    assert!(
        close(volume(&cone), exact / 3.0, 0.001),
        "{}",
        volume(&cone)
    );
    let open = make("msh.tube", &[("caps", json!(false))]);
    assert!(!watertight(&open));
    let flipped = make("msh.tube", &[("height", json!(-1.0))]);
    assert!(volume(&flipped) > 0.0, "upside down still faces outward");
}

#[test]
fn torus_volume() {
    let t = make(
        "msh.torus",
        &[
            ("radius", json!(1.0)),
            ("thickness", json!(0.25)),
            ("rows", json!(64)),
            ("columns", json!(128)),
        ],
    );
    assert!(watertight(&t));
    let exact = 2.0 * std::f64::consts::PI.powi(2) * 1.0 * 0.0625;
    assert!(close(volume(&t), exact, 0.01), "{}", volume(&t));
    assert!(unit_normals(&t, 1e-5));
}

#[test]
fn circle_and_line() {
    let c = make("msh.circle", &[("divisions", json!(8))]);
    assert_eq!((c.points(), c.prims()), (8, 1));
    assert!(c.prim_normal(0)[2] > 0.0);
    let o = make(
        "msh.circle",
        &[("type", json!("polyline")), ("divisions", json!(8))],
    );
    assert_eq!(o.prim(0).len(), 9);
    assert!(!o.closed[0]);
    let pts = make("msh.circle", &[("type", json!("points"))]);
    assert_eq!(pts.prims(), 0);
    let l = make(
        "msh.line",
        &[("points", json!(5)), ("end", json!([0.0, 2.0, 0.0]))],
    );
    assert_eq!(l.prim(0), [0, 1, 2, 3, 4]);
    assert_eq!(l.p[2], [0.0, 1.0, 0.0]);
}

#[test]
fn normals_modes() {
    let b = make("msh.box", &[]);
    let flat = on("msh.normals", &[("mode", json!("flat"))], &b);
    assert_eq!(flat.points(), 24, "every face has its own corners");
    assert!(unit_normals(&flat, 1e-6));
    // Splitting points keeps the shape (volume) but opens it by index.
    assert!(close(volume(&flat), 1.0, 1e-6));
    let cusp = on("msh.normals", &[("cusp", json!(60.0))], &b);
    assert_eq!(cusp.points(), 24, "90-degree edges are sharp at 60");
    let n = cusp.vec3("N").unwrap();
    assert!(
        n.iter()
            .all(|v| v.iter().filter(|c| c.abs() > 0.99).count() == 1)
    );
    let smooth = on("msh.normals", &[("cusp", json!(100.0))], &b);
    assert_eq!(smooth.points(), 8);
    let prim = on("msh.normals", &[("mode", json!("prim"))], &b);
    assert_eq!(prim.prim_attr("N").unwrap().len(), 6);
    let rev = on("msh.normals", &[("reverse", json!(true))], &b);
    let (a, r) = (smooth.vec3("N").unwrap(), rev.vec3("N").unwrap());
    assert_eq!(a[0].map(|x| -x), r[0]);
}

#[test]
fn triangulate_handles_concave_polygons() {
    let b = make("msh.box", &[]);
    let t = on("msh.triangulate", &[], &b);
    assert_eq!(t.prims(), 12);
    assert!(watertight(&t));
    assert!(close(volume(&t), 1.0, 1e-9));
    // An L shape (concave): 6 corners, 4 triangles, same area.
    let mut l = atd_ops_msh::Mesh::new();
    for p in [
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
        [1.0, 2.0, 0.0],
        [0.0, 2.0, 0.0],
    ] {
        l.add_point(p);
    }
    l.add_polygon(&[0, 1, 2, 3, 4, 5]);
    let t = on("msh.triangulate", &[], &l);
    assert_eq!(t.prims(), 4);
    assert!(close(area(&t), 3.0, 1e-9), "{}", area(&t));
    assert!((0..t.prims()).all(|f| t.prim_normal(f)[2] > 0.0));
}

#[test]
fn transform_moves_and_mirrors() {
    let b = make("msh.box", &[]);
    let t = on(
        "msh.transform",
        &[
            ("translate", json!([1.0, 0.0, 0.0])),
            ("scale", json!([2.0, 1.0, 1.0])),
            ("rotate", json!([0.0, 0.0, 90.0])),
        ],
        &b,
    );
    let (lo, hi) = t.bounds().unwrap();
    assert!(
        (lo[0] - 0.5).abs() < 1e-6 && (hi[0] - 1.5).abs() < 1e-6,
        "{lo:?} {hi:?}"
    );
    assert!((lo[1] + 1.0).abs() < 1e-6 && (hi[1] - 1.0).abs() < 1e-6);
    assert!(close(volume(&t), 2.0, 1e-6));
    assert!(unit_normals(&t, 1e-5));
    let m = on("msh.transform", &[("scale", json!([-1.0, 1.0, 1.0]))], &b);
    assert!(volume(&m) > 0.0, "a mirror still faces outward");
}

#[test]
fn merge_joins_attributes_and_groups() {
    let a = make("msh.grid", &[("rows", json!(2)), ("columns", json!(2))]);
    let a = on(
        "msh.group",
        &[("name", json!("left")), ("method", json!("all"))],
        &a,
    );
    let b = make("msh.box", &[]);
    let m = run("msh.merge", &[], &[Some(a.clone()), None, Some(b.clone())]);
    assert_eq!(m.points(), 4 + 8);
    assert_eq!(m.prims(), 1 + 6);
    // Box points have no uv: zeros.
    let uv = m.point_attr("uv").unwrap();
    assert_eq!(uv.len(), 12);
    assert_eq!(edit_count(&m, "left"), 1);
    assert_eq!(m.prim(1)[0], 4, "the box's points follow the grid's");
}

fn edit_count(m: &atd_ops_msh::Mesh, g: &str) -> usize {
    atd_ops_msh::edit::group_count(m, AttrClass::Prim, g)
}

#[test]
fn group_and_delete() {
    let b = make("msh.box", &[("divisions", json!(2))]);
    let top = on(
        "msh.group",
        &[
            ("name", json!("top")),
            ("method", json!("normal")),
            ("direction", json!([0.0, 1.0, 0.0])),
            ("angle", json!(10.0)),
        ],
        &b,
    );
    assert_eq!(edit_count(&top, "top"), 4);
    let open = on("msh.delete", &[("group", json!("top"))], &top);
    assert_eq!(open.prims(), 20);
    assert_eq!(open.points(), 26 - 1, "the centre of the top face is gone");
    let only = on(
        "msh.delete",
        &[("group", json!("top")), ("invert", json!(true))],
        &top,
    );
    assert_eq!((only.prims(), only.points()), (4, 9));
    let boxed = on(
        "msh.group",
        &[
            ("name", json!("low")),
            ("class", json!("point")),
            ("box_centre", json!([0.0, -0.5, 0.0])),
            ("box_size", json!([2.0, 0.1, 2.0])),
        ],
        &b,
    );
    let cut = on(
        "msh.delete",
        &[("group", json!("low")), ("class", json!("point"))],
        &boxed,
    );
    assert_eq!(cut.points(), 26 - 9);
    assert_eq!(cut.prims(), 24 - 4 - 8);
    let err = atd_ops_msh::run("msh.delete", &[("group", json!("nope"))], &[Some(b)]).unwrap_err();
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn fuse_welds_split_points() {
    let b = make("msh.box", &[]);
    let flat = on("msh.normals", &[("mode", json!("flat"))], &b);
    let fused = on("msh.fuse", &[("distance", json!(1e-4))], &flat);
    assert_eq!(fused.points(), 8);
    assert!(watertight(&fused));
    let exact = on("msh.fuse", &[("distance", json!(0.0))], &flat);
    assert_eq!(exact.points(), 8);
}
