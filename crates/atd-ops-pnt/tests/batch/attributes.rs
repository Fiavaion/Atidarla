//! Attribute, Attribute Math, Randomize and Colour by Attribute.

use serde_json::json;

use crate::*;

#[test]
fn attribute_writes_constants_ramps_and_noise_of_any_type() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let line = line3(&mut b);
    let scale = b.after(
        grid,
        "pnt.attribute",
        &[("value", json!([0.5, 9.0, 9.0, 9.0]))],
    );
    let int = b.after(
        grid,
        "pnt.attribute",
        &[
            ("name", json!("n")),
            ("type", json!("int")),
            ("value", json!([-2.6, 0, 0, 0])),
        ],
    );
    let ramp = b.after(
        grid,
        "pnt.attribute",
        &[
            ("name", json!("w")),
            ("type", json!("vec2")),
            ("source", json!("ramp")),
            ("value", json!([0.0, 1.0, 0.0, 0.0])),
            ("value2", json!([1.0, 3.0, 0.0, 0.0])),
        ],
    );
    let index = b.after(
        line,
        "pnt.attribute",
        &[
            ("name", json!("t")),
            ("source", json!("index")),
            ("value", json!([0.0, 0.0, 0.0, 0.0])),
            ("value2", json!([10.0, 0.0, 0.0, 0.0])),
        ],
    );
    let noise = b.after(
        grid,
        "pnt.attribute",
        &[
            ("name", json!("Cd")),
            ("type", json!("vec4")),
            ("source", json!("noise")),
            ("value", json!([0.0, 0.0, 0.0, 0.0])),
            ("frequency", json!(0.37)),
        ],
    );
    let flat = b.after(
        grid,
        "pnt.attribute",
        &[
            ("name", json!("P")),
            ("type", json!("vec3")),
            ("value", json!([1.0, 2.0, 3.0, 0.0])),
        ],
    );
    // Noise of P written into P (reads its own values).
    let jitter = b.after(
        grid,
        "pnt.attribute",
        &[
            ("name", json!("P")),
            ("type", json!("vec3")),
            ("source", json!("noise")),
            ("value", json!([0.0, 0.0, 0.0, 0.0])),
            ("frequency", json!(0.37)),
        ],
    );
    b.done();
    let all = [grid, line, scale, int, ramp, index, noise, flat, jitter];
    cook(&mut e, &all, 0);
    assert_rows(&floats(&e, scale, "pscale"), &[[0.5]; 4], 0.0);
    assert_eq!(words(&e, int, "n"), [(-3i32) as u32; 4]);
    // x = −1 → (0, 1); x = 1 → (1, 3).
    assert_rows(
        &floats(&e, ramp, "w"),
        &[[0.0, 1.0], [1.0, 3.0], [0.0, 1.0], [1.0, 3.0]],
        1e-6,
    );
    assert_rows(&floats(&e, index, "t"), &[[0.0], [5.0], [10.0]], 1e-6);
    let n = floats(&e, noise, "Cd");
    assert!(n.iter().flatten().any(|c| c.abs() > 1e-3), "{n:?}");
    assert!(n.iter().flatten().all(|c| c.abs() < 1.5), "{n:?}");
    assert_ne!(n[0], n[3]);
    assert_rows(&floats(&e, flat, "P"), &[[1.0, 2.0, 3.0]; 4], 0.0);
    // Noise at P: the same field as `Cd`'s first three components.
    let j = floats(&e, jitter, "P");
    for (p, c) in j.iter().zip(&n) {
        assert!(close(p, &c[..3], 1e-6), "{p:?} vs {c:?}");
    }
}

#[test]
fn attribute_math_combines_attributes_and_constants() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let scaled = b.after(
        grid,
        "pnt.attribute_math",
        &[
            ("a", json!("P")),
            ("operation", json!("multiply")),
            ("constant", json!([2.0, 3.0, 1.0, 1.0])),
        ],
    );
    let len = b.after(
        grid,
        "pnt.attribute_math",
        &[
            ("a", json!("P")),
            ("operation", json!("length")),
            ("result", json!("r")),
        ],
    );
    let fit = b.after(
        grid,
        "pnt.attribute_math",
        &[
            ("a", json!("P")),
            ("operation", json!("fit")),
            ("range", json!([-1.0, 1.0])),
            ("constant", json!([0.0, 0.0, 0.0, 0.0])),
            ("constant2", json!([10.0, 20.0, 0.0, 0.0])),
            ("result", json!("f")),
        ],
    );
    let fall = b.after(
        grid,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -50.0, 0.0]))],
    );
    let sum = b.after(
        fall,
        "pnt.attribute_math",
        &[
            ("a", json!("P")),
            ("operation", json!("add")),
            ("b_from", json!("attribute")),
            ("b", json!("v")),
        ],
    );
    let bad = b.after(
        grid,
        "pnt.attribute_math",
        &[("a", json!("P")), ("operation", json!("length"))],
    );
    b.done();
    e.set_frame(0);
    e.cook(&[scaled, len, fit, sum, bad], 0.0).unwrap();
    for n in [scaled, len, fit, sum] {
        assert_eq!(e.error(n), None);
    }
    assert_rows(
        &floats(&e, scaled, "P"),
        &[
            [-2.0, -3.0, 0.0],
            [2.0, -3.0, 0.0],
            [-2.0, 3.0, 0.0],
            [2.0, 3.0, 0.0],
        ],
        0.0,
    );
    let r = 2f32.sqrt();
    assert_rows(&floats(&e, len, "r"), &[[r]; 4], 1e-6);
    assert_rows(
        &floats(&e, fit, "f"),
        &[
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 20.0, 0.0],
            [10.0, 20.0, 0.0],
        ],
        1e-5,
    );
    assert_rows(
        &floats(&e, sum, "P"),
        &[
            [-1.0, -2.0, 0.0],
            [1.0, -2.0, 0.0],
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
        ],
        1e-6,
    );
    assert!(
        e.error(bad).unwrap().contains("would replace an operand"),
        "{:?}",
        e.error(bad)
    );
}

#[test]
fn randomize_is_seeded_and_follows_ids() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let many = b.node(
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(4000))],
    );
    let uni = b.after(
        many,
        "pnt.randomize",
        &[
            ("name", json!("u")),
            ("type", json!("vec2")),
            ("min", json!([2.0, -1.0, 0.0, 0.0])),
            ("max", json!([3.0, 1.0, 0.0, 0.0])),
        ],
    );
    let uni2 = b.after(
        many,
        "pnt.randomize",
        &[
            ("name", json!("u")),
            ("type", json!("vec2")),
            ("min", json!([2.0, -1.0, 0.0, 0.0])),
            ("max", json!([3.0, 1.0, 0.0, 0.0])),
        ],
    );
    let normal = b.after(
        many,
        "pnt.randomize",
        &[
            ("name", json!("g")),
            ("type", json!("float")),
            ("distribution", json!("normal")),
            ("min", json!([5.0, 0.0, 0.0, 0.0])),
            ("max", json!([2.0, 0.0, 0.0, 0.0])),
        ],
    );
    let dir = b.after(
        many,
        "pnt.randomize",
        &[
            ("name", json!("d")),
            ("type", json!("vec3")),
            ("distribution", json!("direction")),
            ("max", json!([2.0, 2.0, 2.0, 0.0])),
        ],
    );
    // Removing points does not change the survivors' values (seeded by id).
    let fewer = b.after(many, "pnt.subset", &[("nth", json!(3))]);
    let after = b.after(
        fewer,
        "pnt.randomize",
        &[
            ("name", json!("u")),
            ("type", json!("vec2")),
            ("min", json!([2.0, -1.0, 0.0, 0.0])),
            ("max", json!([3.0, 1.0, 0.0, 0.0])),
        ],
    );
    b.done();
    let all = [many, uni, uni2, normal, dir, fewer, after];
    cook(&mut e, &all, 0);
    let u = floats(&e, uni, "u");
    assert_eq!(u, floats(&e, uni2, "u"));
    assert!(
        u.iter()
            .all(|v| (2.0..3.0).contains(&v[0]) && (-1.0..1.0).contains(&v[1]))
    );
    let mean = u.iter().map(|v| v[0]).sum::<f32>() / u.len() as f32;
    assert!((mean - 2.5).abs() < 0.03, "{mean}");
    let g: Vec<f32> = floats(&e, normal, "g").iter().map(|v| v[0]).collect();
    let m = g.iter().sum::<f32>() / g.len() as f32;
    let sd = (g.iter().map(|x| (x - m).powi(2)).sum::<f32>() / g.len() as f32).sqrt();
    assert!(
        (m - 5.0).abs() < 0.15 && (sd - 2.0).abs() < 0.15,
        "mean {m}, sd {sd}"
    );
    for d in floats(&e, dir, "d") {
        let l = d.iter().map(|c| c * c).sum::<f32>().sqrt();
        assert!((l - 2.0).abs() < 1e-4, "{d:?}");
    }
    let picked = floats(&e, after, "u");
    for (k, v) in picked.iter().enumerate() {
        assert_eq!(v, &u[k * 3], "point {k}");
    }
}

#[test]
fn colour_maps_an_attribute_through_a_ramp() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let two = b.after(
        grid,
        "pnt.colour",
        &[
            ("colour1", json!([1.0, 0.0, 0.0, 1.0])),
            ("colour2", json!([0.0, 0.0, 1.0, 0.5])),
        ],
    );
    let heat = b.after(
        grid,
        "pnt.colour",
        &[("ramp", json!("heat")), ("measure", json!("x"))],
    );
    let three = b.after(
        grid,
        "pnt.colour",
        &[
            ("ramp", json!("three")),
            ("measure", json!("length")),
            ("low", json!(0.0)),
            ("high", json!(2.0 * 2f64.sqrt())),
        ],
    );
    b.done();
    cook(&mut e, &[two, heat, three], 0);
    assert_rows(
        &floats(&e, two, "Cd"),
        &[
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 0.5],
            [0.0, 0.0, 1.0, 0.5],
        ],
        1e-6,
    );
    assert_rows(
        &floats(&e, heat, "Cd"),
        &[
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        ],
        1e-6,
    );
    // Halfway: the middle colour (the default Colour 2).
    assert_rows(&floats(&e, three, "Cd"), &[[1.0, 0.3, 0.1, 1.0]; 4], 1e-5);
}
