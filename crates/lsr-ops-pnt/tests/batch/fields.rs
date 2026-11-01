//! Forces, Collide, Integrate and the deformers.

use serde_json::json;

use crate::*;

#[test]
fn forces_add_known_accelerations_times_the_step() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let line = line3(&mut b);
    let gravity = b.after(
        line,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -10.0, 0.0]))],
    );
    let attract = b.after(line, "pnt.attract", &[("strength", json!(2.0))]);
    let constant = b.after(
        line,
        "pnt.attract",
        &[("strength", json!(1.0)), ("mode", json!("constant"))],
    );
    let vortex = b.after(
        line,
        "pnt.vortex",
        &[
            ("strength", json!(2.0)),
            ("pull", json!(1.0)),
            ("lift", json!(3.0)),
        ],
    );
    let drag = b.after(gravity, "pnt.drag", &[("amount", json!(1.0))]);
    let wind = b.after(
        line,
        "pnt.wind",
        &[
            ("velocity", json!([2.0, 0.0, 0.0])),
            ("response", json!(5.0)),
            ("gust", json!(0.0)),
        ],
    );
    let still = b.after(line, "pnt.turbulence", &[("amplitude", json!(0.0))]);
    let turb = b.after(line, "pnt.turbulence", &[("amplitude", json!(3.0))]);
    let turb2 = b.after(line, "pnt.turbulence", &[("amplitude", json!(3.0))]);
    b.done();
    let all = [
        line, gravity, attract, constant, vortex, drag, wind, still, turb, turb2,
    ];
    cook(&mut e, &all, 0);
    let g = -10.0 * DT;
    assert_rows(
        &floats(&e, gravity, "v"),
        &[&[0.0, g, 0.0], &[0.0, g, 0.0], &[0.0, g, 0.0]],
        1e-6,
    );
    // Spring: 2 × (0 − x) × dt; nothing at the centre.
    assert_rows(
        &floats(&e, attract, "v"),
        &[&[2.0 * DT, 0.0, 0.0], &[0.0; 3], &[-2.0 * DT, 0.0, 0.0]],
        1e-6,
    );
    assert_rows(
        &floats(&e, constant, "v"),
        &[&[DT, 0.0, 0.0], &[0.0; 3], &[-DT, 0.0, 0.0]],
        1e-6,
    );
    // About +Z: at +X the swirl is +Y, the pull −X, the lift +Z (none on the axis).
    assert_rows(
        &floats(&e, vortex, "v"),
        &[
            &[DT, -2.0 * DT, 3.0 * DT],
            &[0.0; 3],
            &[-DT, 2.0 * DT, 3.0 * DT],
        ],
        1e-6,
    );
    let k = (-DT).exp();
    assert_rows(&floats(&e, drag, "v"), &[[0.0, g * k, 0.0]; 3], 1e-6);
    let w = 2.0 * (1.0 - (-5.0 * DT).exp());
    assert_rows(&floats(&e, wind, "v"), &[[w, 0.0, 0.0]; 3], 1e-6);
    assert!(floats(&e, still, "v").iter().flatten().all(|c| *c == 0.0));
    let t = floats(&e, turb, "v");
    assert_eq!(t, floats(&e, turb2, "v"), "deterministic");
    assert!(t.iter().flatten().any(|c| c.abs() > 1e-4), "{t:?}");
    // P is shared, not copied.
    assert!(
        e.geometry(gravity)
            .unwrap()
            .point("P")
            .unwrap()
            .shares(e.geometry(line).unwrap().point("P").unwrap())
    );
}

#[test]
fn collide_and_integrate_move_points_and_reflect_velocity() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    // Three points at y = −2, under a floor at y = −1, falling.
    let below = b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(3)),
            ("size", json!([2.0, 0.0, 0.0])),
            ("centre", json!([0.0, -2.0, 0.0])),
        ],
    );
    let fall = b.after(
        below,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -10.0, 0.0]))],
    );
    let floor = b.after(
        fall,
        "pnt.collide",
        &[("bounce", json!(0.5)), ("friction", json!(0.0))],
    );
    let ball = b.after(
        below,
        "pnt.collide",
        &[
            ("shape", json!("sphere_outside")),
            ("centre", json!([0.0, -2.0, 0.0])),
            ("radius", json!(0.5)),
        ],
    );
    let inside = b.after(
        below,
        "pnt.collide",
        &[
            ("shape", json!("sphere_inside")),
            ("centre", json!([0.0, -2.0, 0.0])),
            ("radius", json!(0.5)),
        ],
    );
    let step = b.after(fall, "pnt.integrate", &[]);
    b.done();
    let all = [below, fall, floor, ball, inside, step];
    cook(&mut e, &all, 0);
    let g = -10.0 * DT;
    assert_rows(
        &floats(&e, floor, "P"),
        &[&[-1.0, -1.0, 0.0], &[0.0, -1.0, 0.0], &[1.0, -1.0, 0.0]],
        1e-6,
    );
    assert_rows(&floats(&e, floor, "v"), &[[0.0, -0.5 * g, 0.0]; 3], 1e-6);
    // Outside a ball of 0.5 at the middle point: only the middle one moves
    // (to the top, +Y, the fallback direction at the centre).
    assert_rows(
        &floats(&e, ball, "P"),
        &[&[-1.0, -2.0, 0.0], &[0.0, -1.5, 0.0], &[1.0, -2.0, 0.0]],
        1e-6,
    );
    // Inside it: the outer two are pulled onto the sphere.
    assert_rows(
        &floats(&e, inside, "P"),
        &[&[-0.5, -2.0, 0.0], &[0.0, -2.0, 0.0], &[0.5, -2.0, 0.0]],
        1e-6,
    );
    assert_rows(
        &floats(&e, step, "P"),
        &[
            &[-1.0, -2.0 + g * DT, 0.0],
            &[0.0, -2.0 + g * DT, 0.0],
            &[1.0, -2.0 + g * DT, 0.0],
        ],
        1e-6,
    );
}

#[test]
fn deformers_bend_twist_taper_and_wave_known_points() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let line = line3(&mut b);
    let grid = grid4(&mut b);
    // A half circle over the line's length of 2: the end lands 4/π above
    // the start.
    let bend = b.after(
        line,
        "pnt.bend",
        &[
            ("origin", json!([-1.0, 0.0, 0.0])),
            ("axis", json!([1.0, 0.0, 0.0])),
            ("direction", json!([0.0, 1.0, 0.0])),
            ("angle", json!(180.0)),
            ("length", json!(2.0)),
        ],
    );
    let none = b.after(line, "pnt.bend", &[("angle", json!(0.0))]);
    let twist = b.after(grid, "pnt.twist", &[("angle", json!(90.0))]);
    let taper = b.after(grid, "pnt.taper", &[]);
    let wave = b.after(
        line,
        "pnt.wave",
        &[("amplitude", json!(0.5)), ("wavelength", json!(4.0))],
    );
    let ripple = b.after(
        grid,
        "pnt.wave",
        &[
            ("mode", json!("radial")),
            ("travel", json!([0.0, 0.0, 1.0])),
            ("direction", json!([0.0, 0.0, 1.0])),
            ("amplitude", json!(1.0)),
            ("wavelength", json!(4.0 * 2f64.sqrt())),
        ],
    );
    b.done();
    let all = [line, grid, bend, none, twist, taper, wave, ripple];
    cook(&mut e, &all, 0);
    let pi = std::f32::consts::PI;
    let r = 2.0 / pi;
    assert_rows(
        &floats(&e, bend, "P"),
        &[
            &[-1.0, 0.0, 0.0],
            &[-1.0 + r, r, 0.0],
            &[-1.0, 2.0 * r, 0.0],
        ],
        1e-5,
    );
    assert_eq!(floats(&e, none, "P"), floats(&e, line, "P"));
    // 90° per unit about +Y: y = 1 turns +90°, y = −1 turns −90°.
    assert_rows(
        &floats(&e, twist, "P"),
        &[
            &[0.0, -1.0, -1.0],
            &[0.0, -1.0, 1.0],
            &[0.0, 1.0, 1.0],
            &[0.0, 1.0, -1.0],
        ],
        1e-5,
    );
    // Full width at y = −1, none at y = 1.
    assert_rows(
        &floats(&e, taper, "P"),
        &[
            &[-1.0, -1.0, 0.0],
            &[1.0, -1.0, 0.0],
            &[0.0, 1.0, 0.0],
            &[0.0, 1.0, 0.0],
        ],
        1e-6,
    );
    assert_rows(
        &floats(&e, wave, "P"),
        &[&[-1.0, -0.5, 0.0], &[0.0, 0.0, 0.0], &[1.0, 0.5, 0.0]],
        1e-6,
    );
    // Every grid point is √2 from the axis: a quarter wavelength, the crest.
    for p in floats(&e, ripple, "P") {
        assert!((p[2] - 1.0).abs() < 1e-5, "{p:?}");
    }
}

/// Forces in a particle loop: Particles → Gravity → Collide → `prev`. No
/// live particle ends below the floor, and the run repeats exactly.
#[test]
fn a_particle_loop_with_forces_stays_above_the_floor_and_repeats() {
    let _g = serial();
    let run = || {
        let mut e = engine(32)?;
        let mut b = Build::new(&mut e);
        let sys = b.node(
            "pnt.particles",
            &[
                ("capacity", json!(5000)),
                ("rate", json!(20_000.0)),
                ("life", json!(2.0)),
                ("attract", json!(0.0)),
                ("drag", json!(0.0)),
                ("curl_amplitude", json!(0.0)),
                ("velocity", json!([0.0, 2.0, 0.0])),
            ],
        );
        let gravity = b.after(sys, "pnt.gravity", &[]);
        let floor = b.after(
            gravity,
            "pnt.collide",
            &[("centre", json!([0.0, -0.5, 0.0])), ("bounce", json!(0.6))],
        );
        b.wire(floor, sys, "prev");
        b.done();
        for f in 0..60 {
            cook(&mut e, &[floor], f);
        }
        let p = floats(&e, floor, "P");
        Some((count(&e, floor), p))
    };
    let Some((n, p)) = run() else { return };
    assert!(n > 1000, "{n}");
    assert!(p.iter().all(|p| p[1] >= -0.5 - 1e-5), "above the floor");
    assert!(p.iter().any(|p| p[1] < -0.45), "some reached it");
    assert_eq!(run().unwrap(), (n, p), "repeats exactly");
}
