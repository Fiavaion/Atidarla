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
    atd_ops_dat::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    atd_ops_dat::library(&mut lib);
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
    // Text parameters take no expressions (numbers, toggles, vectors and
    // colours do).
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

/// A Table DAT named `table1` with a header row and two rows.
fn levels_table(e: &mut Engine, rows: &str) -> NodeId {
    add(e, "dat.table", "table1", &[("rows", json!(rows))])
}

#[test]
fn dat_cells_drive_parameters() {
    let Some(mut e) = engine() else { return };
    let table = levels_table(&mut e, "name\tlevel\ndim\t0.25\nfull\t1");
    let (_, level) = white_level(&mut e, "level1");
    expr(&mut e, level, "brightness", "op('table1')['dim', 'level']");
    assert_near(red(&mut e, level, 0.0), 64);
    assert_eq!(e.error(level), None);
    // The DAT is a dependency: editing the table cooks the level again.
    e.edit(
        "rows",
        [Edit::SetParam {
            node: table,
            param: "rows".into(),
            value: Some(Param::constant(json!("name\tlevel\ndim\t0.5\nfull\t1"))),
        }],
    )
    .unwrap();
    assert_near(red(&mut e, level, 0.0), 128);
    // By index, with a row that follows time.
    expr(
        &mut e,
        level,
        "brightness",
        "op('table1')[int(time) % 2 + 1, 1]",
    );
    assert_near(red(&mut e, level, 0.0), 128);
    assert_eq!(red(&mut e, level, 1.0), 255);
    assert_near(red(&mut e, level, 2.0), 128);
    // A missing cell fails clearly and keeps the stored value.
    expr(&mut e, level, "brightness", "op('table1')[5, 'level']");
    assert_eq!(red(&mut e, level, 3.0), 255);
    assert_eq!(
        e.error(level),
        Some(
            "expression for `brightness`: op('table1')[5, 'level']: no such cell; \
             it has 3 rows and 2 columns"
        )
    );
    expr(&mut e, level, "brightness", "op('table1')[0, 'level']");
    red(&mut e, level, 4.0);
    assert_eq!(
        e.error(level),
        Some(
            "expression for `brightness`: op('table1')[0, 'level']: the cell is not a number; \
             it holds 'level'"
        )
    );
    expr(&mut e, level, "brightness", "op('level1_src')[0, 0]");
    red(&mut e, level, 5.0);
    assert_eq!(
        e.error(level),
        Some("expression for `brightness`: op('level1_src'): not a DAT, so it has no cells")
    );
}

/// Rule 2: reading DAT cells adds no allocations per frame, even when the
/// row changes every frame.
#[test]
fn dat_cells_do_not_allocate() {
    let Some(mut e) = engine() else { return };
    levels_table(&mut e, "name\tlevel\ndim\t0.5\nfull\t1\nhalf\t0.75");
    let (_, first) = white_level(&mut e, "level0");
    let mut levels = vec![first];
    let mut prev = first;
    for i in 1..6 {
        let l = add(&mut e, "img.level", &format!("level{i}"), &[]);
        wire(&mut e, prev, l);
        levels.push(l);
        prev = l;
    }
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
    // Baseline: the same rows as plain numbers, every frame.
    for &l in &levels {
        expr(&mut e, l, "brightness", "0.5 + 0.25 * (frame % 3)");
    }
    let baseline = measure(&mut e);
    for (i, &l) in levels.iter().enumerate() {
        let source = match i % 3 {
            0 => "op('table1')[1 + frame % 3, 1]",
            1 => "op('table1')['half', 'level'] * (frame % 2 + 1) / 2",
            _ => "op('table1')[1 + frame % 3, 'level']",
        };
        expr(&mut e, l, "brightness", source);
    }
    let with_cells = measure(&mut e);
    assert!(levels.iter().all(|l| e.error(*l).is_none()));
    // wgpu's own allocations on this thread vary by a few between runs
    // (Metal); one allocation per frame in any one node would be 60. The
    // evaluation itself is held to zero exactly in `atd-core`'s
    // `expr_cell_alloc` test.
    assert!(
        with_cells <= baseline + 30,
        "DAT cells allocated: {with_cells} vs {baseline} over 60 frames"
    );
}

/// Pixels of `node` with any alpha after cooking at `time`.
fn coverage(e: &mut Engine, node: NodeId, time: f64) -> usize {
    e.cook(&[node], time).unwrap();
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap())
        .unwrap()
        .chunks_exact(4)
        .filter(|p| p[3] > 0)
        .count()
}

/// First pixel of `node` after cooking at `time`.
fn pixel(e: &mut Engine, node: NodeId, time: f64) -> [u8; 4] {
    e.cook(&[node], time).unwrap();
    let px = read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap();
    [px[0], px[1], px[2], px[3]]
}

fn bind(e: &mut Engine, node: NodeId, param: &str, stored: Value, src: NodeId, channel: &str) {
    e.edit(
        "bind",
        [Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param::bound(stored, src, channel)),
        }],
    )
    .unwrap();
}

/// White → Transform (transparent outside the input); returns the transform.
fn white_transform(e: &mut Engine, name: &str) -> NodeId {
    let white = add(
        e,
        "img.constant",
        &format!("{name}_src"),
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let t = add(e, "img.transform", name, &[]);
    wire(e, white, t);
    t
}

/// PRD A66: an LFO bound to a vector drives every component, so the
/// transformed image grows and shrinks frame to frame.
#[test]
fn a_channel_drives_every_component_of_a_vector() {
    let Some(mut e) = engine() else { return };
    // 0.75 + 0.25 sin(2π t): 0.75 at t = 0, 1 at 0.25, 0.5 at 0.75.
    let lfo = add(
        &mut e,
        "sig.lfo",
        "lfo1",
        &[("offset", json!(0.75)), ("amplitude", json!(0.25))],
    );
    let t = white_transform(&mut e, "transform1");
    bind(&mut e, t, "scale", json!([1.0, 1.0]), lfo, "lfo");
    let (mid, full, half) = (
        coverage(&mut e, t, 0.0),
        coverage(&mut e, t, 0.25),
        coverage(&mut e, t, 0.75),
    );
    assert_eq!(full, 32, "scale 1 covers the 8x4 image");
    assert!(half < mid && mid < full, "{half} < {mid} < {full}");
    assert_eq!(half, 8, "scale 0.5 covers a quarter");
    // A native expression drives it the same way, without a binding.
    expr_with(
        &mut e,
        t,
        "scale",
        "0.5 if frame % 2 else 1",
        json!([1.0, 1.0]),
    );
    let a = coverage(&mut e, t, 1.0);
    let b = coverage(&mut e, t, 1.1);
    assert_eq!([a.min(b), a.max(b)], [8, 32], "alternates");
    assert_eq!(e.error(t), None);
}

/// A colour takes one number on red, green and blue and keeps its stored
/// alpha; a whole SIG node with three channels drives them one each, a
/// named channel all three.
#[test]
fn expressions_and_channels_drive_a_colour() {
    let Some(mut e) = engine() else { return };
    let c = add(&mut e, "img.constant", "colour1", &[]);
    expr_with(
        &mut e,
        c,
        "colour",
        "0.25 + 0.25",
        json!([0.0, 0.0, 0.0, 1.0]),
    );
    let [r, g, b, a] = pixel(&mut e, c, 0.0);
    for v in [r, g, b] {
        assert_near(v, 128);
    }
    assert_eq!(a, 255, "the stored alpha");
    assert_eq!(e.error(c), None);
    let rgb = add(
        &mut e,
        "sig.constant",
        "rgb",
        &[
            ("name1", json!("r")),
            ("value1", json!(1.0)),
            ("name2", json!("g")),
            ("value2", json!(0.5)),
            ("name3", json!("b")),
            ("value3", json!(0.0)),
        ],
    );
    bind(&mut e, c, "colour", json!([0.0, 0.0, 0.0, 1.0]), rgb, "");
    let [r, g, b, a] = pixel(&mut e, c, 1.0);
    assert_eq!((r, b, a), (255, 0, 255), "per component");
    assert_near(g, 128);
    bind(&mut e, c, "colour", json!([0.0, 0.0, 0.0, 1.0]), rgb, "g");
    let [r, g, b, _] = pixel(&mut e, c, 2.0);
    assert!(r == g && g == b, "one channel on every component");
    assert_near(r, 128);
    // Another expression reads the driven component.
    let (_, level) = white_level(&mut e, "level1");
    expr(
        &mut e,
        level,
        "brightness",
        "op('colour1').par.colour[1] * 2",
    );
    assert_eq!(red(&mut e, level, 3.0), 255);
    expr(
        &mut e,
        level,
        "brightness",
        "op('colour1').par.colour[0] + 0.25",
    );
    assert_near(red(&mut e, level, 4.0), 191);
}

/// Rule 2: a vector driven by a channel or a native expression adds no
/// allocations per frame over the same nodes driven on a number.
#[test]
fn driven_vectors_do_not_allocate() {
    let Some(mut e) = engine() else { return };
    let lfo = add(&mut e, "sig.lfo", "lfo1", &[]);
    let first = white_transform(&mut e, "transform1");
    let second = add(&mut e, "img.transform", "transform2", &[]);
    wire(&mut e, first, second);
    let measure = |e: &mut Engine| {
        let mut t = 0.0;
        for _ in 0..20 {
            e.cook(&[second], t).unwrap();
            t += 1.0 / 60.0;
        }
        COUNTING.with(|c| c.set(true));
        let before = ALLOCS.with(Cell::get);
        for _ in 0..60 {
            e.cook(&[second], t).unwrap();
            t += 1.0 / 60.0;
        }
        let n = ALLOCS.with(Cell::get) - before;
        COUNTING.with(|c| c.set(false));
        n
    };
    // Baseline: numbers driven.
    bind(&mut e, first, "rotate", json!(0.0), lfo, "lfo");
    expr_with(&mut e, second, "rotate", "10 * sin(time)", json!(0.0));
    let baseline = measure(&mut e);
    // The same nodes with vectors driven instead.
    for node in [first, second] {
        e.edit(
            "reset",
            [Edit::SetParam {
                node,
                param: "rotate".into(),
                value: None,
            }],
        )
        .unwrap();
    }
    bind(&mut e, first, "scale", json!([1.0, 1.0]), lfo, "lfo");
    expr_with(
        &mut e,
        second,
        "translate",
        "0.1 * sin(time) + op('transform1').par.scale[0] * 0",
        json!([0.0, 0.0]),
    );
    let vectors = measure(&mut e);
    assert_eq!(e.error(second), None);
    assert!(
        vectors <= baseline,
        "driven vectors allocated: {vectors} vs {baseline} over 60 frames"
    );
}
