//! Neighbour searches (grid hash): Normals, Smooth and Connect, and the
//! lines Render Points draws from Connect.

use serde_json::json;

use crate::*;

fn len(v: &[f32]) -> f32 {
    v.iter().take(3).map(|c| c * c).sum::<f32>().sqrt()
}

#[test]
fn normals_of_a_sphere_and_a_plane() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let sphere = b.node(
        "pnt.scatter",
        &[
            ("shape", json!("sphere")),
            ("count", json!(20_000)),
            ("centre", json!([0.5, 0.0, 0.0])),
        ],
    );
    // Drop the exact normals Scatter wrote, then estimate them.
    let est = b.after(
        sphere,
        "pnt.normals",
        &[("radius", json!(0.1)), ("centre", json!([0.5, 0.0, 0.0]))],
    );
    let sheet = b.node(
        "pnt.scatter",
        &[
            ("shape", json!("rectangle")),
            ("count", json!(5000)),
            ("rotate", json!([30.0, 0.0, 0.0])),
        ],
    );
    let flat = b.after(
        sheet,
        "pnt.normals",
        &[("radius", json!(0.1)), ("orient", json!("toward"))],
    );
    let flat2 = b.after(
        sheet,
        "pnt.normals",
        &[("radius", json!(0.1)), ("orient", json!("toward"))],
    );
    b.done();
    cook(&mut e, &[sphere, est, sheet, flat, flat2], 0);
    let exact = floats(&e, sphere, "P");
    let n = floats(&e, est, "N");
    let mut worst = 1.0f32;
    for (p, n) in exact.iter().zip(&n) {
        let r = [p[0] - 0.5, p[1], p[2]];
        let c = (r[0] * n[0] + r[1] * n[1] + r[2] * n[2]) / len(&r);
        worst = worst.min(c);
        assert!((len(n) - 1.0).abs() < 1e-4, "{n:?}");
    }
    assert!(
        worst > 0.97,
        "estimated normals point outward along the radius: worst cos {worst}"
    );
    // A tilted plane: every normal is its normal, toward +Z.
    let a = 30f32.to_radians();
    let want = [0.0, -a.sin(), a.cos()];
    let f = floats(&e, flat, "N");
    for n in &f {
        assert!(close(n, &want, 2e-3), "{n:?} vs {want:?}");
    }
    assert_eq!(f, floats(&e, flat2, "N"), "deterministic");
}

#[test]
fn smooth_averages_neighbours() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let line = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(21)),
            ("size", json!([2.0, 0.0, 0.0])),
        ],
    );
    // A step in `w`: 0 left of x = 0, 1 from it on.
    let step = b.after(
        line,
        "pnt.attribute",
        &[
            ("name", json!("w")),
            ("source", json!("ramp")),
            ("start", json!([-0.0001, 0.0, 0.0])),
            ("end", json!([0.0, 0.0, 0.0])),
            ("value", json!([0.0, 0.0, 0.0, 0.0])),
            ("value2", json!([1.0, 0.0, 0.0, 0.0])),
        ],
    );
    let once = b.after(
        step,
        "pnt.smooth",
        &[("attribute", json!("w")), ("radius", json!(0.15))],
    );
    let more = b.after(
        step,
        "pnt.smooth",
        &[
            ("attribute", json!("w")),
            ("radius", json!(0.15)),
            ("iterations", json!(4)),
        ],
    );
    let none = b.after(
        step,
        "pnt.smooth",
        &[
            ("attribute", json!("w")),
            ("radius", json!(0.15)),
            ("strength", json!(0.0)),
        ],
    );
    b.done();
    cook(&mut e, &[step, once, more, none], 0);
    let w0: Vec<f32> = floats(&e, step, "w").iter().map(|v| v[0]).collect();
    assert_eq!(w0[9], 0.0);
    assert_eq!(w0[10], 1.0);
    let w1: Vec<f32> = floats(&e, once, "w").iter().map(|v| v[0]).collect();
    // Neighbours at 0.1 (weight (1 − 0.44)²), self weight 1.
    let k = (1.0f32 - 0.01 / 0.0225).powi(2);
    let left = k / (1.0 + 2.0 * k);
    assert!((w1[9] - left).abs() < 1e-5, "{} vs {left}", w1[9]);
    assert!((w1[10] - (1.0 - left)).abs() < 1e-5, "{}", w1[10]);
    assert_eq!(w1[0], 0.0, "far from the step: unchanged");
    assert_eq!(w1[20], 1.0);
    let w4: Vec<f32> = floats(&e, more, "w").iter().map(|v| v[0]).collect();
    assert!(
        w4[8] > w1[8] && w4[8] < 0.5,
        "spreads further: {} vs {}",
        w4[8],
        w1[8]
    );
    let wn: Vec<f32> = floats(&e, none, "w").iter().map(|v| v[0]).collect();
    assert_eq!(wn, w0);
}

#[test]
fn connect_links_nearest_neighbours_and_render_points_draws_them() {
    let _g = serial();
    let Some(mut e) = engine(32) else { return };
    let mut b = Build::new(&mut e);
    let line = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(5)),
            ("size", json!([1.6, 0.0, 0.0])),
        ],
    );
    // Points 0.4 apart: each links to the next only (unique, radius 0.5).
    let link = b.after(
        line,
        "pnt.connect",
        &[("radius", json!(0.5)), ("links", json!(1))],
    );
    let both = b.after(
        line,
        "pnt.connect",
        &[
            ("radius", json!(0.5)),
            ("links", json!(2)),
            ("unique", json!(false)),
        ],
    );
    let r = b.node(
        "img.render_points",
        &[("draw", json!("lines")), ("colour_from", json!("constant"))],
    );
    b.wire(link, r, "geo");
    let dots = b.node("img.render_points", &[("colour_from", json!("constant"))]);
    b.wire(link, dots, "geo");
    b.done();
    cook(&mut e, &[link, both, r, dots], 0);
    let g = e.geometry(link).unwrap();
    let t = g.topology().unwrap();
    assert_eq!(g.prims(), 5);
    let read = |a: &atd_gpu::geo::Attribute| -> Vec<u32> {
        atd_gpu::geo::readback::read_blocking(e.device(), e.queue(), e.buffer_pool(), a)
            .unwrap()
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    };
    assert_eq!(
        read(&t.corners),
        [0, 1, 1, 2, 2, 3, 3, 4, 4, 4],
        "the last has no higher neighbour"
    );
    assert_eq!(read(&t.start), [0, 2, 4, 6, 8, 10]);
    assert_eq!(read(&t.kinds), [1; 5]);
    assert_eq!(words(&e, link, "near"), [1, 2, 3, 4, 4]);
    let d: Vec<f32> = floats(&e, link, "neardist").iter().map(|v| v[0]).collect();
    assert!(
        d[..4].iter().all(|d| (d - 0.4).abs() < 1e-6) && (d[4] - 0.5).abs() < 1e-6,
        "{d:?}"
    );
    // Both directions, two links: the ends have one neighbour within 0.5
    // (a degenerate second line), the inner points both of theirs.
    let c2 = read(&e.geometry(both).unwrap().topology().unwrap().corners);
    let mut links: Vec<[u32; 2]> = c2
        .chunks_exact(4)
        .map(|c| {
            assert_eq!(c[0], c[2], "both lines start at their point");
            let mut l = [c[1], c[3]];
            l.sort_unstable();
            l
        })
        .collect();
    links.dedup();
    assert_eq!(links, [[0, 1], [0, 2], [1, 3], [2, 4], [3, 4]]);
    // The lines light the pixel row through y = 0 between the end points;
    // sprites alone leave the gaps between points dark (x = −0.6: column 6).
    let lit = |n, x: usize| -> bool {
        let px = pixels(&e, n);
        (14..18).any(|y| px[y * 32 + x][0] > 0)
    };
    let row = (5..27).filter(|x| lit(r, *x)).count();
    assert!(row >= 20, "a line across: {row} columns");
    assert!(lit(r, 6) && !lit(dots, 6));
    assert!(lit(dots, 3) || lit(dots, 4), "a sprite at x = −0.8");
}
