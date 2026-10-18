//! Scatter, Merge, Copy to Points, Emitter and Sort.

use serde_json::json;

use crate::*;

fn len(v: &[f32]) -> f32 {
    v.iter().take(3).map(|c| c * c).sum::<f32>().sqrt()
}

#[test]
fn scatter_puts_points_on_each_shape() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let shape = |b: &mut Build<'_>, s: &str, extra: &[(&str, serde_json::Value)]| {
        let mut params = vec![("shape", json!(s)), ("count", json!(500))];
        params.extend(extra.iter().cloned());
        b.node("pnt.scatter", &params)
    };
    let line = shape(&mut b, "line", &[]);
    let circle = shape(&mut b, "circle", &[("radius", json!(2.0))]);
    let tilted = shape(&mut b, "circle", &[("rotate", json!([90.0, 0.0, 0.0]))]);
    let disk = shape(&mut b, "disk", &[("inner", json!(0.5))]);
    let rect = shape(&mut b, "rectangle", &[("size", json!([4.0, 1.0, 0.0]))]);
    let sphere = shape(&mut b, "sphere", &[("centre", json!([0.0, 0.0, 3.0]))]);
    let ball = shape(&mut b, "ball", &[]);
    let boxed = shape(&mut b, "box", &[("size", json!([1.0, 2.0, 3.0]))]);
    let again = shape(&mut b, "box", &[("size", json!([1.0, 2.0, 3.0]))]);
    b.done();
    let all = [line, circle, tilted, disk, rect, sphere, ball, boxed, again];
    cook(&mut e, &all, 0);
    let eps = 1e-5;
    for p in floats(&e, line, "P") {
        assert!(
            (-1.0..=1.0).contains(&p[0]) && p[1] == 0.0 && p[2] == 0.0,
            "{p:?}"
        );
    }
    assert!(e.geometry(line).unwrap().point("N").is_none());
    for p in floats(&e, circle, "P") {
        assert!((len(&p) - 2.0).abs() < eps && p[2] == 0.0, "{p:?}");
    }
    for p in floats(&e, tilted, "P") {
        assert!(p[1].abs() < eps && (len(&p) - 1.0).abs() < eps, "{p:?}");
    }
    for n in floats(&e, tilted, "N") {
        assert!(
            close(&n, &[0.0, 1.0, 0.0], eps) || close(&n, &[0.0, -1.0, 0.0], eps),
            "{n:?}"
        );
    }
    for p in floats(&e, disk, "P") {
        let r = len(&p);
        assert!((0.5 - eps..=1.0 + eps).contains(&r), "{p:?}");
    }
    for p in floats(&e, rect, "P") {
        assert!(
            p[0].abs() <= 2.0 && p[1].abs() <= 0.5 && p[2] == 0.0,
            "{p:?}"
        );
    }
    let (sp, sn) = (floats(&e, sphere, "P"), floats(&e, sphere, "N"));
    for (p, n) in sp.iter().zip(&sn) {
        let q = [p[0], p[1], p[2] - 3.0];
        assert!(
            (len(&q) - 1.0).abs() < eps && close(&q, &n[..3], eps),
            "{p:?} {n:?}"
        );
    }
    let bp = floats(&e, ball, "P");
    assert!(bp.iter().all(|p| len(p) <= 1.0 + eps));
    let mean_r = bp.iter().map(|p| len(p)).sum::<f32>() / bp.len() as f32;
    assert!(
        (mean_r - 0.75).abs() < 0.05,
        "uniform in volume: mean radius {mean_r}"
    );
    for p in floats(&e, boxed, "P") {
        assert!(
            p[0].abs() <= 0.5 && p[1].abs() <= 1.0 && p[2].abs() <= 1.5,
            "{p:?}"
        );
    }
    assert_eq!(floats(&e, boxed, "P"), floats(&e, again, "P"), "seeded");
    assert_eq!(words(&e, boxed, "id"), (0..500).collect::<Vec<u32>>());
}

#[test]
fn merge_joins_points_and_fills_missing_attributes() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let line = line3(&mut b);
    let fall = b.after(
        line,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -50.0, 0.0]))],
    );
    let m = b.node("pnt.merge", &[]);
    b.wire(grid, m, "a");
    b.wire(fall, m, "c");
    let sys = b.node(
        "pnt.particles",
        &[
            ("capacity", json!(1000)),
            ("initial", json!(100)),
            ("rate", json!(0.0)),
        ],
    );
    let m2 = b.node("pnt.merge", &[]);
    b.wire(sys, m2, "a");
    b.wire(grid, m2, "b");
    b.done();
    cook(&mut e, &[m, m2, sys], 0);
    assert_eq!(count(&e, m), 7);
    assert!(!e.geometry(m).unwrap().is_gpu_counted());
    assert_rows(
        &floats(&e, m, "P"),
        &[
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
        ],
        0.0,
    );
    assert_eq!(words(&e, m, "id"), [0, 1, 2, 3, 0, 1, 2]);
    let v = floats(&e, m, "v");
    assert!(
        v[..4].iter().flatten().all(|c| *c == 0.0),
        "zero where missing"
    );
    assert_rows(&v[4..], &[[0.0, -1.0, 0.0]; 3], 1e-6);
    // A simulation's count stays on the GPU: 100 particles, then the grid.
    assert!(e.geometry(m2).unwrap().is_gpu_counted());
    assert_eq!(count(&e, m2), 104);
    let p = floats(&e, m2, "P");
    assert_eq!(p[..100], floats(&e, sys, "P")[..]);
    assert_rows(&p[100..], &floats(&e, grid, "P"), 0.0);
}

#[test]
fn copy_places_a_shape_on_every_point() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let line = line3(&mut b);
    let grid = b.node(
        "pnt.points",
        &[
            ("count", json!(4)),
            ("size", json!([2.0, 2.0, 0.0])),
            ("colour", json!([0.5, 0.25, 1.0, 1.0])),
        ],
    );
    let copy = b.node("pnt.copy", &[]);
    b.wire(line, copy, "source");
    b.wire(grid, copy, "points");
    let big = b.after(
        grid,
        "pnt.attribute",
        &[("value", json!([2.0, 0.0, 0.0, 0.0]))],
    );
    let scaled = b.node("pnt.copy", &[]);
    b.wire(line, scaled, "source");
    b.wire(big, scaled, "points");
    let facing = b.after(
        grid,
        "pnt.attribute",
        &[
            ("name", json!("N")),
            ("type", json!("vec3")),
            ("value", json!([1.0, 0.0, 0.0, 0.0])),
        ],
    );
    let turned = b.node("pnt.copy", &[("orient", json!("normal"))]);
    b.wire(line, turned, "source");
    b.wire(facing, turned, "points");
    b.done();
    cook(&mut e, &[copy, scaled, turned], 0);
    assert_eq!(count(&e, copy), 12);
    let p = floats(&e, copy, "P");
    let g = floats(&e, grid, "P");
    for t in 0..4 {
        for s in 0..3 {
            let want = [g[t][0] + s as f32 - 1.0, g[t][1], 0.0];
            assert!(
                close(&p[t * 3 + s], &want, 1e-6),
                "{t} {s}: {:?}",
                p[t * 3 + s]
            );
        }
    }
    assert_eq!(
        words(&e, copy, "copynum"),
        [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3]
    );
    assert_eq!(words(&e, copy, "id"), [0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2]);
    assert_rows(&floats(&e, copy, "Cd"), &[[0.5, 0.25, 1.0, 1.0]; 12], 1e-6);
    let p = floats(&e, scaled, "P");
    assert!(
        close(&p[2], &[-1.0 + 2.0, -1.0, 0.0], 1e-6),
        "pscale 2: {:?}",
        p[2]
    );
    // +Z turned onto +X turns +X onto −Z.
    let p = floats(&e, turned, "P");
    assert!(close(&p[2], &[-1.0, -1.0, -1.0], 1e-5), "{:?}", p[2]);
}

#[test]
fn emitter_appends_particles_from_source_points() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let sys = b.node(
        "pnt.particles",
        &[
            ("capacity", json!(1000)),
            ("rate", json!(0.0)),
            ("attract", json!(0.0)),
            ("drag", json!(0.0)),
            ("curl_amplitude", json!(0.0)),
        ],
    );
    let src = line3(&mut b);
    let emit = b.after(
        sys,
        "pnt.emitter",
        &[
            ("rate", json!(500.0)),
            ("spread", json!(0.0)),
            ("velocity", json!([0.0, 1.0, 0.0])),
            ("life", json!(10.0)),
        ],
    );
    b.wire(src, emit, "source");
    b.wire(emit, sys, "prev");
    b.done();
    for f in 0..5 {
        cook(&mut e, &[emit], f);
        assert_eq!(count(&e, emit), 10 * (f as u32 + 1), "frame {f}");
    }
    let ids = words(&e, emit, "id");
    let base = 1u32 << 30;
    assert_eq!(
        ids,
        (base..base + 50).collect::<Vec<u32>>(),
        "in emission order"
    );
    let p = floats(&e, emit, "P");
    for q in &p[40..] {
        assert!(
            q[1] == 0.0 && [-1.0, 0.0, 1.0].contains(&q[0]),
            "born on a source point: {q:?}"
        );
    }
    // The oldest have moved up for four steps of v = (0, 1, 0).
    assert!(
        p[..10].iter().all(|q| (q[1] - 4.0 * DT).abs() < 1e-5),
        "{:?}",
        &p[..10]
    );
    assert_rows(&floats(&e, emit, "v"), &[[0.0, 1.0, 0.0]; 50], 1e-6);
}

#[test]
fn sort_orders_points_and_carries_attributes() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let many = b.node(
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(1500)),
            ("random_colour", json!(true)),
        ],
    );
    let by_x = b.after(many, "pnt.sort", &[("axis", json!([1.0, 0.0, 0.0]))]);
    let down = b.after(
        many,
        "pnt.sort",
        &[
            ("key", json!("distance")),
            ("point", json!([0.0, 0.0, 0.0])),
            ("descending", json!(true)),
        ],
    );
    let back = b.after(many, "pnt.sort", &[("key", json!("reverse"))]);
    let shuffled = b.after(
        many,
        "pnt.sort",
        &[("key", json!("random")), ("seed", json!(3))],
    );
    let shuffled2 = b.after(
        many,
        "pnt.sort",
        &[("key", json!("random")), ("seed", json!(3))],
    );
    let by_id = b.after(
        shuffled,
        "pnt.sort",
        &[("key", json!("attribute")), ("attribute", json!("id"))],
    );
    b.done();
    let all = [many, by_x, down, back, shuffled, shuffled2, by_id];
    cook(&mut e, &all, 0);
    let p = floats(&e, by_x, "P");
    assert!(p.windows(2).all(|w| w[0][0] <= w[1][0]), "ascending x");
    // Attributes follow: each point keeps its colour.
    let (p0, c0, id0) = (
        floats(&e, many, "P"),
        floats(&e, many, "Cd"),
        words(&e, many, "id"),
    );
    let (cx, idx) = (floats(&e, by_x, "Cd"), words(&e, by_x, "id"));
    for k in 0..p.len() {
        let i = idx[k] as usize;
        assert_eq!((&p[k], &cx[k]), (&p0[i], &c0[i]));
    }
    let d: Vec<f32> = floats(&e, down, "P").iter().map(|p| len(p)).collect();
    assert!(d.windows(2).all(|w| w[0] >= w[1]), "farthest first");
    let rev: Vec<u32> = id0.iter().rev().copied().collect();
    assert_eq!(words(&e, back, "id"), rev);
    let s = words(&e, shuffled, "id");
    assert_eq!(s, words(&e, shuffled2, "id"), "seeded");
    assert_ne!(s, id0);
    let mut sorted = s.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, id0, "a permutation");
    assert_eq!(
        words(&e, by_id, "id"),
        id0,
        "sorting by id undoes the shuffle"
    );
    assert_eq!(floats(&e, by_id, "P"), p0);
}
