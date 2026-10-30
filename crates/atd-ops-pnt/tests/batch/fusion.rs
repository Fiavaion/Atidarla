//! Deformer fusion: a straight run of per-point stages cooked as one pass
//! gives the same buffers, byte for byte, as the run's nodes one by one.

use atd_core::graph::{Edit, NodeId, Param};
use atd_gpu::engine::Engine;
use atd_gpu::geo::readback::read_blocking;
use serde_json::{Value, json};

use crate::*;

/// Every byte of point attribute `name` of `node` (the whole buffer).
fn bytes(e: &Engine, node: NodeId, name: &str) -> Vec<u8> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap()
}

fn animate(b: &mut Build<'_>, node: NodeId, param: &str, expr: &str) {
    b.e.edit(
        "animate",
        [Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param {
                value: if param == "seed" {
                    json!(0)
                } else {
                    json!(0.0)
                },
                expr: Some(expr.into()),
                bind: None,
            }),
        }],
    )
    .unwrap();
}

/// Every deformer and force over a sphere of points with `N` (and `v` made by
/// the first force), animated so every node cooks every frame: Transform,
/// Bend, Twist, Taper, Wave, the six forces, Collide, Integrate, Transform.
fn chain(e: &mut Engine) -> NodeId {
    let mut b = Build::new(e);
    let sphere = b.node(
        "pnt.scatter",
        &[("shape", json!("sphere")), ("count", json!(5000))],
    );
    let ops: [(&str, Vec<(&str, Value)>); 14] = [
        (
            "pnt.transform",
            vec![
                ("rotate", json!([10.0, 20.0, 30.0])),
                ("scale", json!([1.0, 2.0, 0.5])),
                ("translate", json!([0.1, -0.2, 0.3])),
            ],
        ),
        ("pnt.bend", vec![("angle", json!(70.0))]),
        ("pnt.twist", vec![("angle", json!(45.0))]),
        ("pnt.taper", vec![("end", json!(0.4))]),
        ("pnt.wave", vec![("mode", json!("radial"))]),
        ("pnt.gravity", vec![]),
        ("pnt.attract", vec![("mode", json!("inverse_square"))]),
        ("pnt.vortex", vec![]),
        ("pnt.wind", vec![]),
        ("pnt.turbulence", vec![]),
        ("pnt.drag", vec![("mode", json!("quadratic"))]),
        ("pnt.collide", vec![("shape", json!("sphere_inside"))]),
        ("pnt.integrate", vec![]),
        (
            "pnt.transform",
            vec![
                ("rotate", json!([0.0, 45.0, 0.0])),
                ("uniform_scale", json!(1.5)),
            ],
        ),
    ];
    let mut last = sphere;
    let mut nodes = Vec::new();
    for (ty, params) in ops {
        last = b.after(last, ty, &params);
        nodes.push(last);
    }
    b.done();
    let mut b = Build::new(e);
    animate(&mut b, sphere, "radius", "1 + 0.2 * sin(time * 3)");
    animate(&mut b, nodes[4], "phase", "time * 90");
    animate(&mut b, nodes[9], "evolution", "time * 0.5");
    b.done();
    last
}

/// Every byte of `P`, `v` and `N` of the two engines' `a` and `b` agrees.
fn same_bytes(x: &Engine, a: NodeId, y: &Engine, b: NodeId, what: &str) {
    for name in ["P", "v", "N"] {
        let has = x.geometry(a).unwrap().point(name).is_some();
        assert_eq!(
            has,
            y.geometry(b).unwrap().point(name).is_some(),
            "{what}: `{name}`"
        );
        if has {
            assert!(
                bytes(x, a, name) == bytes(y, b, name),
                "{what}: `{name}` differs"
            );
        }
    }
}

#[test]
fn a_fused_run_gives_the_unfused_bytes() {
    let _g = serial();
    let (Some(mut fused), Some(mut plain)) = (engine(8), engine(8)) else {
        return;
    };
    plain.set_fusion(false);
    let end_f = chain(&mut fused);
    let end_p = chain(&mut plain);
    for f in 0..6 {
        cook(&mut fused, &[end_f], f);
        cook(&mut plain, &[end_p], f);
        // Two runs: Transform to Vortex (eight) and Drag to Transform (four);
        // Wind and Turbulence are not fused.
        assert_eq!(fused.fused_last_frame(), 12, "frame {f}");
        assert_eq!(plain.fused_last_frame(), 0);
        same_bytes(&fused, end_f, &plain, end_p, &format!("frame {f}"));
    }
}

/// Thirty Transforms and repeats of every fused deformer: still the same
/// bytes (a stage kind used many times in one kernel).
#[test]
fn repeated_stages_give_the_unfused_bytes() {
    let _g = serial();
    let (Some(mut fused), Some(mut plain)) = (engine(8), engine(8)) else {
        return;
    };
    plain.set_fusion(false);
    let build = |e: &mut Engine| {
        let mut b = Build::new(e);
        let mut last = b.node(
            "pnt.scatter",
            &[("shape", json!("ball")), ("count", json!(3000))],
        );
        let first = last;
        for k in 0..30 {
            last = b.after(
                last,
                "pnt.transform",
                &[("rotate", json!([1.0, 2.0, 3.0 + k as f64]))],
            );
        }
        for ty in ["pnt.bend", "pnt.twist", "pnt.taper", "pnt.wave"] {
            for _ in 0..3 {
                last = b.after(last, ty, &[]);
            }
        }
        b.done();
        let mut b = Build::new(e);
        animate(&mut b, first, "radius", "1 + 0.1 * sin(time)");
        b.done();
        last
    };
    let (a, b) = (build(&mut fused), build(&mut plain));
    for f in 0..3 {
        cook(&mut fused, &[a], f);
        cook(&mut plain, &[b], f);
        assert_eq!(fused.fused_last_frame(), 42);
        same_bytes(&fused, a, &plain, b, &format!("frame {f}"));
    }
}

/// Live, the fused pipeline is built on a worker thread: the run cooks
/// unfused until it is ready (the engine is pending meanwhile), then fused,
/// with the same result.
#[test]
fn live_fusion_waits_for_its_pipeline() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    e.set_mode(atd_gpu::engine::CookMode::Live);
    let mut b = Build::new(&mut e);
    let src = grid4(&mut b);
    let a = b.after(src, "pnt.twist", &[]);
    let c = b.after(a, "pnt.bend", &[]);
    b.done();
    let mut b = Build::new(&mut e);
    animate(&mut b, src, "seed", "frame");
    b.done();
    let mut fused = false;
    for f in 0..2000 {
        cook(&mut e, &[c], f);
        if e.fused_last_frame() == 2 {
            fused = true;
            break;
        }
        assert!(e.pending(), "unfused only while the pipeline is built");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(fused, "the fused pipeline arrived");
    assert!(!e.pending());
    assert_eq!(count(&e, c), 4);
}

/// A node another one reads, or a viewed node, ends a run; so does a node
/// that is not a stage (Delete).
#[test]
fn runs_stop_where_a_result_is_seen() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let src = grid4(&mut b);
    let a = b.after(src, "pnt.twist", &[]);
    let c = b.after(a, "pnt.taper", &[]);
    let d = b.after(c, "pnt.delete", &[]);
    let x = b.after(d, "pnt.wave", &[]);
    let y = b.after(x, "pnt.bend", &[]);
    b.done();
    let mut b = Build::new(&mut e);
    animate(&mut b, src, "seed", "frame");
    b.done();
    cook(&mut e, &[y], 0);
    // Twist + Taper, and Wave + Bend.
    assert_eq!(e.fused_last_frame(), 4);
    cook(&mut e, &[y, a], 1);
    // Twist is viewed: its result is kept, so only Wave + Bend fuse.
    assert_eq!(e.fused_last_frame(), 2);
    assert_eq!(count(&e, a), 4);
}
