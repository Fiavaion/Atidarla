//! For-Each blocks: Begin → body → End, End wired back into Begin's `end`.

use atd_core::graph::NodeId;
use serde_json::{Value, json};

use crate::*;

/// `source` → For-Each (`params`) → body → End → back into `end`; the body
/// is Attribute Math adding `iteration` to `P` (so pass k moves by k on X).
fn block(b: &mut Build<'_>, source: NodeId, params: &[(&str, Value)]) -> (NodeId, NodeId) {
    let begin = b.after(source, "pnt.foreach", params);
    let shift = b.after(
        begin,
        "pnt.attribute_math",
        &[
            ("a", json!("P")),
            ("operation", json!("add")),
            ("b_from", json!("attribute")),
            ("b", json!("iteration")),
        ],
    );
    let end = b.after(shift, "pnt.foreach_end", &[]);
    b.wire(end, begin, "end");
    (begin, end)
}

#[test]
fn foreach_count_runs_the_body_per_pass_and_merges() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let (_, end) = block(&mut b, grid, &[("iterations", json!(3))]);
    b.done();
    for f in 0..3 {
        // The same every frame: the passes start over.
        cook(&mut e, &[end], f);
        assert_eq!(count(&e, end), 12);
        let mut want = Vec::new();
        for k in 0..3 {
            let x = k as f32;
            want.extend([
                [-1.0 + x, -1.0, 0.0],
                [1.0 + x, -1.0, 0.0],
                [-1.0 + x, 1.0, 0.0],
                [1.0 + x, 1.0, 0.0],
            ]);
        }
        assert_rows(&floats(&e, end, "P"), &want, 1e-6);
        assert_eq!(
            words(&e, end, "iteration"),
            [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2]
        );
        assert_eq!(words(&e, end, "id"), [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]);
    }
    assert!(!e.geometry(end).unwrap().is_gpu_counted());
}

#[test]
fn foreach_pieces_gives_pass_k_piece_k() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    // Pieces of two points: {0, 1} and {2, 3}; a third pass finds none.
    let pieces = b.after(grid, "pnt.piece", &[("size", json!(2))]);
    let (_, end) = block(
        &mut b,
        pieces,
        &[("mode", json!("pieces")), ("iterations", json!(3))],
    );
    b.done();
    for f in 0..2 {
        cook(&mut e, &[end], f);
        assert_eq!(count(&e, end), 4);
        assert_eq!(words(&e, end, "id"), [0, 1, 2, 3]);
        assert_eq!(words(&e, end, "iteration"), [0, 0, 1, 1]);
        assert_rows(
            &floats(&e, end, "P"),
            &[
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [0.0, 1.0, 0.0],
                [2.0, 1.0, 0.0],
            ],
            1e-6,
        );
    }
    assert!(e.geometry(end).unwrap().is_gpu_counted());
}

/// Only pieces 0 to Iterations − 1 are passed; a bad piece attribute is a
/// node error.
#[test]
fn foreach_pieces_are_bounded_and_checked() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let pieces = b.after(grid, "pnt.piece", &[("size", json!(1))]);
    let (_, end) = block(
        &mut b,
        pieces,
        &[("mode", json!("pieces")), ("iterations", json!(2))],
    );
    let (bad, _) = block(
        &mut b,
        grid,
        &[("mode", json!("pieces")), ("attribute", json!("P"))],
    );
    b.done();
    cook(&mut e, &[end], 0);
    assert_eq!(words(&e, end, "id"), [0, 1]);
    e.set_frame(1);
    e.cook(&[bad], 0.0).unwrap();
    assert!(e.error(bad).unwrap().contains("one component"));
}

#[test]
fn foreach_end_outside_a_block_passes_its_input() {
    let _g = serial();
    let Some(mut e) = engine(8) else { return };
    let mut b = Build::new(&mut e);
    let grid = grid4(&mut b);
    let end = b.after(grid, "pnt.foreach_end", &[]);
    b.done();
    cook(&mut e, &[grid, end], 0);
    assert_eq!(floats(&e, end, "P"), floats(&e, grid, "P"));
    assert_eq!(words(&e, end, "id"), [0, 1, 2, 3]);
}
