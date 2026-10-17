//! Parameter expressions in the engine (FR-P2, PRD A22): the native subset
//! on the cook path, with dependencies, time, errors and no allocation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

struct Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 8,
        height: 4,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, name: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty, name),
    }];
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits).unwrap();
    id
}

fn expr(e: &mut Engine, node: NodeId, param: &str, source: &str) {
    expr_with(e, node, param, source, json!(1.0));
}

/// An expression with `stored` as the parameter's own value.
fn expr_with(e: &mut Engine, node: NodeId, param: &str, source: &str, stored: Value) {
    e.edit(
        "expr",
        [Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param {
                value: stored,
                expr: Some(source.into()),
                bind: None,
            }),
        }],
    )
    .unwrap();
}

fn wire(e: &mut Engine, from: NodeId, to: NodeId) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, "in")),
        }],
    )
    .unwrap();
}

/// Red of the first pixel of `node` after cooking at `time`.
fn red(e: &mut Engine, node: NodeId, time: f64) -> u8 {
    e.cook(&[node], time).unwrap();
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()[0]
}

/// Levels that fall exactly between two 8-bit values (0.5 is 127.5) round up
/// on Metal and WARP and down on NVIDIA: accept either.
fn assert_near(got: u8, want: u8) {
    assert!(
        got.abs_diff(want) <= 1,
        "level {got}, expected {want} +/- 1"
    );
}

/// White → Level; returns (constant, level).
fn white_level(e: &mut Engine, name: &str) -> (NodeId, NodeId) {
    let white = add(
        e,
        "img.constant",
        &format!("{name}_src"),
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let level = add(e, "img.level", name, &[]);
    wire(e, white, level);
    (white, level)
}

#[test]
fn time_frame_and_me_digits() {
    let Some(mut e) = engine() else { return };
    let (_, level) = white_level(&mut e, "level4");
    expr(&mut e, level, "brightness", "0.25 if time < 1 else 0.75");
    assert_eq!(red(&mut e, level, 0.0), 64);
    // `time` makes the node cook every frame without any edit.
    assert_eq!(red(&mut e, level, 1.5), 191);
    expr(&mut e, level, "brightness", "me.digits / 8");
    assert_near(red(&mut e, level, 2.0), 128);
    // A toggle: invert on even frames.
    expr(&mut e, level, "brightness", "1");
    expr_with(&mut e, level, "invert", "frame % 2 == 0", json!(false));
    let a = red(&mut e, level, 3.0);
    let b = red(&mut e, level, 3.1);
    assert_eq!([a.min(b), a.max(b)], [0, 255], "alternates");
}

#[test]
fn channels_and_parameters_of_other_nodes() {
    let Some(mut e) = engine() else { return };
    let source = add(
        &mut e,
        "sig.constant",
        "knobs",
        &[("name1", json!("x")), ("value1", json!(0.25))],
    );
    let ramp = add(
        &mut e,
        "img.ramp",
        "ramp1",
        &[("colour_b", json!([0.5, 0.0, 0.0, 1.0]))],
    );
    let (_, a) = white_level(&mut e, "a");
    let (_, b) = white_level(&mut e, "b");
    expr(&mut e, a, "brightness", "op('knobs')['x'] * 2");
    // b follows a's parameter, and a vector component of the ramp's.
    expr(
        &mut e,
        b,
        "brightness",
        "op('a').par.brightness + op('ramp1').par.colour_b[0]",
    );
    assert_near(red(&mut e, a, 0.0), 128);
    assert_eq!(red(&mut e, b, 0.0), 255);
    // Changing the source recooks the readers, though nothing is wired.
    e.edit(
        "x",
        [Edit::SetParam {
            node: source,
            param: "value1".into(),
            value: Some(Param::constant(json!(0.1))),
        }],
    )
    .unwrap();
    assert_eq!(red(&mut e, a, 1.0), 51);
    assert_near(red(&mut e, b, 1.0), 179);
    // Renaming the source breaks the reference: an error, the stored value.
    e.edit(
        "rename",
        [Edit::Rename {
            node: source,
            name: "dials".into(),
        }],
    )
    .unwrap();
    assert_eq!(red(&mut e, a, 2.0), 255, "stored value 1.0");
    assert_eq!(
        e.error(a),
        Some("expression for `brightness`: op('knobs'): no such node")
    );
    expr(&mut e, a, "brightness", "op('dials')['x'] * 2");
    assert_eq!(red(&mut e, a, 3.0), 51);
    assert_eq!(e.error(a), None);
    let _ = ramp;
}

#[test]
fn errors_show_on_the_node_and_keep_the_value() {
    let Some(mut e) = engine() else { return };
    let (_, level) = white_level(&mut e, "level1");
    expr(&mut e, level, "brightness", "1 / (time - time)");
    assert_eq!(red(&mut e, level, 0.0), 255, "the stored value");
    assert_eq!(
        e.error(level),
        Some("expression for `brightness`: division by zero")
    );
    expr(&mut e, level, "brightness", "0.5");
    assert_near(red(&mut e, level, 1.0), 128);
    assert_eq!(e.error(level), None);
    // Two parameters referring to each other.
    let (_, other) = white_level(&mut e, "level2");
    expr(&mut e, level, "brightness", "op('level2').par.brightness");
    expr(&mut e, other, "brightness", "op('level1').par.brightness");
    red(&mut e, level, 2.0);
    assert_eq!(
        e.error(level),
        Some("expression for `brightness`: parameters refer to each other in a loop")
    );
    // Not native and no Python worker here.
    expr(&mut e, level, "brightness", "len('ab') / 2");
    red(&mut e, level, 3.0);
    assert!(
        e.error(level).unwrap().contains("needs Python"),
        "{:?}",
        e.error(level)
    );
    // Only number and toggle parameters take expressions.
    let text = add(&mut e, "img.text", "text1", &[]);
    let err = e.edit(
        "expr",
        [Edit::SetParam {
            node: text,
            param: "text".into(),
            value: Some(Param {
                value: json!("x"),
                expr: Some("1".into()),
                bind: None,
            }),
        }],
    );
    assert!(err.is_err());
}

/// Rule 2: native expressions add no allocations per frame.
#[test]
fn native_expressions_do_not_allocate() {
    let Some(mut e) = engine() else { return };
    let lfo = add(&mut e, "sig.lfo", "lfo1", &[]);
    let mut levels = Vec::new();
    let (src, first) = white_level(&mut e, "level0");
    levels.push(first);
    let mut prev = first;
    for i in 1..8 {
        let l = add(&mut e, "img.level", &format!("level{i}"), &[]);
        wire(&mut e, prev, l);
        levels.push(l);
        prev = l;
    }
    let _ = (src, lfo);
    let out = prev;
    let measure = |e: &mut Engine| {
        let mut t = 0.0;
        for _ in 0..20 {
            e.cook(&[out], t).unwrap();
            t += 1.0 / 60.0;
        }
        COUNTING.with(|c| c.set(true));
        let before = ALLOCS.with(Cell::get);
        for _ in 0..60 {
            e.cook(&[out], t).unwrap();
            t += 1.0 / 60.0;
        }
        let n = ALLOCS.with(Cell::get) - before;
        COUNTING.with(|c| c.set(false));
        n
    };
    // Baseline: the LFO bound to every level (cooks every frame).
    for &l in &levels {
        e.edit(
            "bind",
            [Edit::SetParam {
                node: l,
                param: "brightness".into(),
                value: Some(Param::bound(json!(1.0), lfo, "lfo")),
            }],
        )
        .unwrap();
    }
    let baseline = measure(&mut e);
    // The same through expressions reading time, a channel and parameters.
    for (i, &l) in levels.iter().enumerate() {
        let source = match i % 3 {
            0 => "0.9 + 0.1 * sin(time * 3) * (me.digits % 2)",
            1 => "clamp(op('lfo1')['lfo'] * 0.5 + 0.75, 0, 1)",
            _ => "op('level0').par.brightness if frame % 2 else 1",
        };
        expr(&mut e, l, "brightness", source);
    }
    let with_exprs = measure(&mut e);
    assert!(levels.iter().all(|l| e.error(*l).is_none()));
    assert!(
        with_exprs <= baseline,
        "expressions allocated: {with_exprs} vs {baseline} over 60 frames"
    );
}
