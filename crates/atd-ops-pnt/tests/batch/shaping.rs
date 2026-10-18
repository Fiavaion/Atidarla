//! Transform, Delete, Subset, Group and Age.

use serde_json::json;

use crate::*;

#[test]
fn transform_moves_points_and_turns_velocities() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let fall = b.after(
        grid,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -10.0, 0.0]))],
    );
    let t = b.after(
        fall,
        "pnt.transform",
        &[
            ("translate", json!([1.0, 0.0, 0.0])),
            ("rotate", json!([0.0, 0.0, 90.0])),
            ("uniform_scale", json!(2.0)),
        ],
    );
    let pivot = b.after(
        grid,
        "pnt.transform",
        &[
            ("scale", json!([0.5, 0.5, 0.5])),
            ("pivot", json!([1.0, 1.0, 0.0])),
        ],
    );
    b.done();
    cook(&mut e, &[grid, fall, t, pivot], 0);
    // Scale 2, then (x, y) → (−y, x), then +X.
    assert_rows(
        &floats(&e, t, "P"),
        &[
            [3.0, -2.0, 0.0],
            [3.0, 2.0, 0.0],
            [-1.0, -2.0, 0.0],
            [-1.0, 2.0, 0.0],
        ],
        1e-5,
    );
    assert_rows(&floats(&e, t, "v"), &[[0.4, 0.0, 0.0]; 4], 1e-5);
    assert_rows(
        &floats(&e, pivot, "P"),
        &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
        1e-6,
    );
}

#[test]
fn delete_and_subset_remove_points_in_order() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let corner = [
        ("centre", json!([1.0, 1.0, 0.0])),
        ("size", json!([1.0, 1.0, 1.0])),
    ];
    let boxed = b.after(grid, "pnt.delete", &corner);
    let kept = b.after(
        grid,
        "pnt.delete",
        &[
            corner[0].clone(),
            corner[1].clone(),
            ("invert", json!(true)),
        ],
    );
    let ball = b.after(
        grid,
        "pnt.delete",
        &[
            ("rule", json!("sphere")),
            ("centre", json!([-1.0, -1.0, 0.0])),
            ("radius", json!(0.1)),
        ],
    );
    let right = b.after(
        grid,
        "pnt.delete",
        &[
            ("rule", json!("attribute")),
            ("attribute", json!("P")),
            ("compare", json!(">")),
            ("value", json!(0.0)),
        ],
    );
    let even = b.after(grid, "pnt.subset", &[("nth", json!(2))]);
    let odd = b.after(
        grid,
        "pnt.subset",
        &[("nth", json!(2)), ("offset", json!(1))],
    );
    let range = b.after(
        grid,
        "pnt.subset",
        &[
            ("rule", json!("range")),
            ("first", json!(1)),
            ("count", json!(2)),
        ],
    );
    let many = b.node(
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(2000))],
    );
    let half = b.after(
        many,
        "pnt.subset",
        &[
            ("rule", json!("random")),
            ("fraction", json!(0.25)),
            ("seed", json!(7)),
        ],
    );
    let half2 = b.after(
        many,
        "pnt.subset",
        &[
            ("rule", json!("random")),
            ("fraction", json!(0.25)),
            ("seed", json!(7)),
        ],
    );
    // Delete of a GPU-counted input (a particle system).
    let sys = b.node(
        "pnt.particles",
        &[
            ("capacity", json!(1000)),
            ("initial", json!(800)),
            ("rate", json!(0.0)),
        ],
    );
    let cut = b.after(
        sys,
        "pnt.delete",
        &[
            ("rule", json!("attribute")),
            ("attribute", json!("P")),
            ("value", json!(0.0)),
        ],
    );
    b.done();
    let all = [
        grid, boxed, kept, ball, right, even, odd, range, many, half, half2, sys, cut,
    ];
    cook(&mut e, &all, 0);
    let ids = |n| words(&e, n, "id");
    assert_eq!(ids(boxed), [0, 1, 2]);
    assert_eq!(ids(kept), [3]);
    assert_eq!(ids(ball), [1, 2, 3]);
    assert_eq!(ids(right), [0, 2]);
    assert_eq!(ids(even), [0, 2]);
    assert_eq!(ids(odd), [1, 3]);
    assert_eq!(ids(range), [1, 2]);
    // Every attribute follows its point.
    assert_rows(
        &floats(&e, right, "P"),
        &[[-1.0, -1.0, 0.0], [-1.0, 1.0, 0.0]],
        0.0,
    );
    assert_eq!(e.geometry(right).unwrap().points(), 4, "the capacity");
    let n = count(&e, half);
    assert!((400..600).contains(&n), "about a quarter: {n}");
    assert_eq!(ids(half), ids(half2), "seeded");
    let w = ids(half);
    assert!(w.windows(2).all(|p| p[0] < p[1]), "order kept");
    let (all_p, left) = (count(&e, sys), count(&e, cut));
    assert_eq!(all_p, 800);
    assert!(left > 300 && left < 500, "about half of 800: {left}");
    assert!(floats(&e, cut, "P").iter().all(|p| p[0] <= 0.0));
}

#[test]
fn group_bits_feed_delete() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let top = b.after(
        grid,
        "pnt.group",
        &[
            ("name", json!("top")),
            ("rule", json!("attribute")),
            ("attribute", json!("P")),
            ("component", json!(1)),
            ("value", json!(0.0)),
        ],
    );
    let right = b.after(
        top,
        "pnt.group",
        &[
            ("name", json!("right")),
            ("rule", json!("attribute")),
            ("attribute", json!("P")),
            ("value", json!(0.0)),
        ],
    );
    // Top-right only: intersect "right" with "top".
    let both = b.after(
        right,
        "pnt.group",
        &[
            ("name", json!("right")),
            ("rule", json!("group")),
            ("group", json!("top")),
            ("operation", json!("intersect")),
        ],
    );
    let del = b.after(
        both,
        "pnt.delete",
        &[("rule", json!("group")), ("group", json!("right"))],
    );
    let missing = b.after(
        grid,
        "pnt.delete",
        &[("rule", json!("group")), ("group", json!("nope"))],
    );
    b.done();
    e.set_frame(0);
    e.cook(&[del, right, missing], 0.0).unwrap();
    assert_eq!(e.error(del), None);
    // Bit 0 top (ids 2, 3), bit 1 right (ids 1, 3).
    assert_eq!(words(&e, right, "group"), [0, 2, 1, 3]);
    assert_eq!(words(&e, del, "id"), [0, 1, 2]);
    let g = e.geometry(del).unwrap();
    let groups: Vec<&str> = g
        .groups(atd_core::geo::AttrClass::Point)
        .iter()
        .map(|n| &**n)
        .collect();
    assert_eq!(groups, ["top", "right"]);
    assert!(
        e.error(missing).unwrap().contains("no point group `nope`"),
        "{:?}",
        e.error(missing)
    );
}

#[test]
fn age_counts_time_and_removes_by_rules() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let aged = b.after(grid, "pnt.age", &[("kill_dead", json!(false))]);
    let dead = b.after(grid, "pnt.age", &[("life", json!(0.001))]);
    let region = b.after(
        grid,
        "pnt.age",
        &[
            ("region", json!("inside_sphere")),
            ("centre", json!([1.0, 1.0, 0.0])),
            ("radius", json!(0.5)),
        ],
    );
    let fall = b.after(
        grid,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -1.0, 0.0]))],
    );
    let slow = b.after(fall, "pnt.age", &[("min_speed", json!(1.0))]);
    b.done();
    cook(&mut e, &[aged, dead, region, slow], 0);
    assert_rows(&floats(&e, aged, "age"), &[[DT]; 4], 1e-7);
    assert!(
        !e.geometry(aged).unwrap().is_gpu_counted(),
        "nothing removed"
    );
    assert_eq!(count(&e, dead), 0);
    assert_eq!(words(&e, region, "id"), [0, 1, 2]);
    assert_eq!(count(&e, slow), 0, "all slower than 1");
}
