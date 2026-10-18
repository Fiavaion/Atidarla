//! Solver (a simulation loop's entry), a Solver Block as a Pack, and Piece
//! (For-Each Begin) with Randomize keyed by piece.

use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use atd_core::pack::{PackDef, PackInput, PackOutput, PackSet};
use atd_core::registry::Registry;
use serde_json::json;

use crate::*;

/// Grid → Solver → Gravity (−10) → Integrate → back into the Solver.
fn falling(b: &mut Build<'_>) -> (NodeId, NodeId) {
    let grid = grid4(b);
    let solver = b.node("pnt.solver", &[]);
    b.wire(grid, solver, "init");
    let gravity = b.after(
        solver,
        "pnt.gravity",
        &[("acceleration", json!([0.0, -10.0, 0.0]))],
    );
    let step = b.after(gravity, "pnt.integrate", &[]);
    b.wire(step, solver, "prev");
    (solver, step)
}

#[test]
fn a_solver_loop_integrates_and_resets() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let (solver, step) = falling(&mut b);
    b.done();
    for f in 0..5 {
        cook(&mut e, &[step], f);
    }
    // After five steps: v = −10 dt × 5, y = y0 − 10 dt² (1 + … + 5).
    let v = floats(&e, step, "v");
    assert_rows(&v, &[[0.0, -50.0 * DT, 0.0]; 4], 1e-5);
    let p = floats(&e, step, "P");
    let dy = -10.0 * DT * DT * 15.0;
    assert_rows(
        &p,
        &[
            [-1.0, -1.0 + dy, 0.0],
            [1.0, -1.0 + dy, 0.0],
            [-1.0, 1.0 + dy, 0.0],
            [1.0, 1.0 + dy, 0.0],
        ],
        1e-5,
    );
    // Reset: back to the start (one step from init).
    e.edit(
        "reset",
        [Edit::SetParam {
            node: solver,
            param: "reset".into(),
            value: Some(Param::constant(json!(true))),
        }],
    )
    .unwrap();
    cook(&mut e, &[step], 5);
    assert_rows(&floats(&e, step, "v"), &[[0.0, -10.0 * DT, 0.0]; 4], 1e-6);
}

#[test]
fn a_solver_restarts_when_its_body_removes_everything() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let solver = b.node("pnt.solver", &[("start_frame", json!(2))]);
    b.wire(grid, solver, "init");
    // Points live 3.5 steps.
    let age = b.after(solver, "pnt.age", &[("life", json!(3.5 * DT))]);
    b.wire(age, solver, "prev");
    b.done();
    let mut counts = Vec::new();
    for f in 0..10 {
        cook(&mut e, &[age], f);
        counts.push(count(&e, age));
    }
    // Frames 0-2 restart from init (Start Frame 2); then ages dt, 2dt, 3dt
    // live and 4dt dies. The count is on the GPU, so the Solver does not see
    // that the state is empty: it stays empty until Reset.
    assert_eq!(counts, [4, 4, 4, 4, 4, 0, 0, 0, 0, 0]);
}

/// The falling loop as a Pack (a Solver Block): flattening keeps the
/// inner feedback wire, and the result equals the top-level loop's.
#[test]
fn a_solver_block_pack_cooks_like_the_loop() {
    let _g = serial();
    let mut base = Registry::default();
    atd_ops_pnt::register(&mut base);
    let mut def = PackDef::new("fall");
    def.summary = "A falling Solver Block.".into();
    let mut inner = Network::default();
    let n = |id: u64, ty: &str| Edit::AddNode {
        node: Node::new(NodeId(id), ty, format!("n{id}")),
    };
    let wire = |a: u64, b: u64, s: &str| Edit::Connect {
        wire: Wire::new(Endpoint::new(NodeId(a), "out"), Endpoint::new(NodeId(b), s)),
    };
    for edit in [
        n(0, "pnt.solver"),
        n(1, "pnt.gravity"),
        n(2, "pnt.integrate"),
        Edit::SetParam {
            node: NodeId(1),
            param: "acceleration".into(),
            value: Some(Param::constant(json!([0.0, -10.0, 0.0]))),
        },
        wire(0, 1, "in"),
        wire(1, 2, "in"),
        wire(2, 0, "prev"),
    ] {
        inner.apply(&base, edit).unwrap();
    }
    def.network = inner;
    def.inputs = vec![PackInput {
        name: "init".into(),
        targets: vec![Endpoint::new(NodeId(0), "init")],
    }];
    def.outputs = vec![PackOutput {
        name: "out".into(),
        source: Endpoint::new(NodeId(2), "out"),
    }];
    let mut packs = PackSet::default();
    packs.insert(def.clone());
    let registry = packs.register(&base).unwrap();
    let mut top = Network::default();
    top.apply(
        &registry,
        Edit::AddNode {
            node: Node::new(NodeId(0), "pnt.points", "grid"),
        },
    )
    .unwrap();
    for (param, v) in [("count", json!(4)), ("size", json!([2.0, 2.0, 0.0]))] {
        top.apply(
            &registry,
            Edit::SetParam {
                node: NodeId(0),
                param: param.into(),
                value: Some(Param::constant(v)),
            },
        )
        .unwrap();
    }
    top.apply(
        &registry,
        Edit::AddNode {
            node: Node::new(NodeId(1), def.type_id(), "block"),
        },
    )
    .unwrap();
    top.apply(
        &registry,
        Edit::Connect {
            wire: Wire::new(
                Endpoint::new(NodeId(0), "out"),
                Endpoint::new(NodeId(1), "init"),
            ),
        },
    )
    .unwrap();
    let flat = packs.flatten(&top, &registry).unwrap();
    let out = flat.view(NodeId(1)).unwrap();
    let Some(mut e) = engine(8) else { return };
    e.load(flat.network.clone()).unwrap();
    for f in 0..5 {
        cook(&mut e, &[out], f);
    }
    let packed = floats(&e, out, "P");
    let Some(mut e2) = engine(8) else { return };
    let mut b = Build::new(&mut e2);
    let (_, step) = falling(&mut b);
    b.done();
    for f in 0..5 {
        cook(&mut e2, &[step], f);
    }
    assert_eq!(packed, floats(&e2, step, "P"));
}

#[test]
fn piece_numbers_points_and_randomize_keys_on_it() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let chunks = b.after(grid, "pnt.piece", &[("size", json!(3))]);
    let line = line3(&mut b);
    let copy = b.node("pnt.copy", &[]);
    b.wire(line, copy, "source");
    b.wire(grid, copy, "points");
    let pieces = b.after(copy, "pnt.piece", &[("rule", json!("attribute"))]);
    let tint = b.after(pieces, "pnt.randomize", &[("key", json!("piece"))]);
    let cells = b.after(
        grid,
        "pnt.piece",
        &[("rule", json!("grid")), ("cell", json!(1.5))],
    );
    let bad = b.after(grid, "pnt.randomize", &[("key", json!("P"))]);
    b.done();
    e.set_frame(0);
    e.cook(&[chunks, tint, cells, bad], 0.0).unwrap();
    for n in [chunks, tint, cells] {
        assert_eq!(e.error(n), None);
    }
    assert_eq!(words(&e, chunks, "piece"), [0, 0, 0, 1]);
    assert_eq!(
        words(&e, pieces, "piece"),
        [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3]
    );
    let c = floats(&e, tint, "Cd");
    for t in 0..4 {
        assert_eq!(c[t * 3], c[t * 3 + 1]);
        assert_eq!(c[t * 3], c[t * 3 + 2], "one colour per piece");
    }
    assert_ne!(c[0], c[3]);
    // Cells of 1.5: (−1, −1) is in cell (−1, −1), (1, 1) in (0, 0): four cells.
    let w = words(&e, cells, "piece");
    let mut u = w.clone();
    u.sort_unstable();
    u.dedup();
    assert_eq!(u.len(), 4, "{w:?}");
    assert!(e.error(bad).unwrap().contains("one component"));
}
