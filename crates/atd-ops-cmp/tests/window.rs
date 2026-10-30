//! GPU tests for `cmp.window` in the engine: the identity map passes the
//! input through at the output's resolution, slices and soft edges reach
//! the image, a bad description is a node error naming its line and a
//! fixed one recovers, test patterns replace the input, and steady-state
//! frames allocate within the IMG bound and create no textures.

use std::alloc::{GlobalAlloc, Layout, System};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

struct Counting;
thread_local! {
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

const W: u32 = 64;
const H: u32 = 32;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    atd_ops_cmp::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    atd_ops_cmp::library(&mut lib);
    let config = EngineConfig {
        width: W,
        height: H,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

macro_rules! engine_or_skip {
    () => {
        match engine() {
            Some(e) => e,
            None => {
                let required =
                    std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
                assert!(
                    !required,
                    "ATD_REQUIRE_GPU is set but there is no GPU adapter"
                );
                eprintln!("skipping: no GPU adapter");
                return;
            }
        }
    };
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name(ty.split('.').nth(1).unwrap());
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

fn set(e: &mut Engine, node: NodeId, p: &str, v: Value) {
    e.edit(
        "set",
        [Edit::SetParam {
            node,
            param: p.into(),
            value: Some(Param::constant(v)),
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

fn image(e: &mut Engine, node: NodeId) -> (u32, u32, Vec<u8>) {
    e.cook(&[node], 0.0).unwrap();
    assert_eq!(e.error(node), None);
    let t = e.result(node).expect("result").clone();
    let px = read_rgba8(e.device(), e.queue(), &t).unwrap();
    (t.width(), t.height(), px)
}

fn px(img: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.0 + x) * 4) as usize;
    img.2[i..i + 4].try_into().unwrap()
}

#[test]
fn the_identity_map_passes_the_input_through() {
    let mut e = engine_or_skip!();
    let ramp = add(&mut e, "img.ramp", &[]);
    let win = add(
        &mut e,
        "cmp.window",
        &[("width", json!(0)), ("height", json!(0))],
    );
    wire(&mut e, ramp, win);
    let a = image(&mut e, ramp);
    let b = image(&mut e, win);
    assert_eq!((b.0, b.1), (W, H), "0 × 0 is the project resolution");
    let worst = a.2.iter().zip(&b.2).map(|(x, y)| x.abs_diff(*y)).max();
    assert!(worst <= Some(1), "largest difference {worst:?}");
    assert_eq!(e.status(win), Some("1 slice · 64×32"));
    // The output takes its own resolution.
    set(&mut e, win, "width", json!(40));
    set(&mut e, win, "height", json!(20));
    let c = image(&mut e, win);
    assert_eq!((c.0, c.1), (40, 20));
    assert_eq!(e.status(win), Some("1 slice · 40×20"));
}

#[test]
fn slices_and_soft_edges_reach_the_image() {
    let mut e = engine_or_skip!();
    let white = add(
        &mut e,
        "img.constant",
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let win = add(
        &mut e,
        "cmp.window",
        &[
            ("width", json!(128)),
            ("height", json!(16)),
            (
                "slices",
                json!(
                    "[slice left]\ndst = 0 0 0.5 1\n\n[slice right]\ndst = 0.5 0 0.5 1\nblend = 0.5 0 0 0\nblend_gamma = 1\n"
                ),
            ),
        ],
    );
    wire(&mut e, white, win);
    let img = image(&mut e, win);
    assert_eq!(px(&img, 10, 8), [255; 4]);
    // The right slice ramps up over its left half: 0.5 at a quarter.
    let q = px(&img, 64 + 16, 8)[0];
    assert!((120..=136).contains(&q), "{q}");
    assert!(px(&img, 65, 8)[0] < 10);
    assert_eq!(px(&img, 120, 8), [255; 4]);
    assert_eq!(e.status(win), Some("2 slices · 128×16"));
}

#[test]
fn a_bad_description_is_a_node_error_until_fixed() {
    let mut e = engine_or_skip!();
    let win = add(
        &mut e,
        "cmp.window",
        &[("slices", json!("[slice a]\nwarp = twist"))],
    );
    e.cook(&[win], 0.0).unwrap();
    let err = e.error(win).expect("an error").to_owned();
    assert!(err.starts_with("slices: line 2: unknown warp"), "{err}");
    set(
        &mut e,
        win,
        "slices",
        json!("[slice a]\nwarp = perspective"),
    );
    e.cook(&[win], 0.0).unwrap();
    assert_eq!(e.error(win), None);
}

#[test]
fn test_patterns_replace_the_input() {
    let mut e = engine_or_skip!();
    let black = add(
        &mut e,
        "img.constant",
        &[("colour", json!([0.0, 0.0, 0.0, 1.0]))],
    );
    let win = add(
        &mut e,
        "cmp.window",
        &[
            ("width", json!(64)),
            ("height", json!(32)),
            ("pattern", json!("bars")),
        ],
    );
    wire(&mut e, black, win);
    let img = image(&mut e, win);
    assert_eq!(px(&img, 2, 16), [191, 191, 191, 255], "75% white bar");
    set(&mut e, win, "pattern", json!("off"));
    let img = image(&mut e, win);
    assert_eq!(px(&img, 2, 16), [0, 0, 0, 255]);
}

#[test]
fn steady_state_allocates_within_the_img_bound() {
    let mut e = engine_or_skip!();
    let noise = add(&mut e, "img.noise", &[]);
    let lfo = add(&mut e, "sig.lfo", &[]);
    e.edit(
        "bind",
        [Edit::SetParam {
            node: noise,
            param: "z".into(),
            value: Some(Param::bound(json!(0.0), lfo, "lfo")),
        }],
    )
    .unwrap();
    let win = add(
        &mut e,
        "cmp.window",
        &[
            ("width", json!(64)),
            ("height", json!(32)),
            (
                "slices",
                json!(
                    "[slice a]\nwarp = catmull_rom\ngrid = 3 3\nblend = 0.1 0.1 0 0\nmask = 0 0 1 0 1 1\n"
                ),
            ),
        ],
    );
    wire(&mut e, noise, win);
    // A typical one-pass IMG operator on the same source, for scale.
    let level = add(&mut e, "img.level", &[]);
    wire(&mut e, noise, level);
    let mut t = 0.0;
    for _ in 0..30 {
        e.cook(&[win, level], t).unwrap();
        t += 1.0 / 60.0;
    }
    e.cook(&[noise], t).unwrap();
    // Allocations per frame cooking the source alone, then with the window.
    let mut per_frame = |e: &mut Engine, node: NodeId, nodes: usize| {
        let frames = 100;
        COUNTING.with(|c| c.set(true));
        let before = ALLOCS.with(std::cell::Cell::get);
        for _ in 0..frames {
            assert_eq!(
                e.cook(&[node], t).unwrap(),
                nodes,
                "every node cooks every frame"
            );
            t += 1.0 / 60.0;
        }
        let n = ALLOCS.with(std::cell::Cell::get) - before;
        COUNTING.with(|c| c.set(false));
        n as f64 / frames as f64
    };
    let source = per_frame(&mut e, noise, 2);
    let one_pass = per_frame(&mut e, level, 3) - source;
    let created = e.pool().created();
    let window = per_frame(&mut e, win, 3) - source;
    assert_eq!(e.pool().created(), created, "no new textures");
    eprintln!("cmp.window steady state: {window:.1} allocations per cook (Level {one_pass:.1})");
    // What remains is wgpu recording one render pass; the window also binds
    // a vertex and an index buffer.
    assert!(
        window <= 1.5 * one_pass + 2.0,
        "{window:.1} allocations per cook against {one_pass:.1} for a one-pass operator"
    );
}
