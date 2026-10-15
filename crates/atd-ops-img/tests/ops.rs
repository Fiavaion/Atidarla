//! GPU unit tests for every IMG operator (rule 7), run on any adapter:
//! lavapipe locally, WARP on Windows CI, Metal on macOS CI.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

const W: u32 = 32;
const H: u32 = 16;

fn engine() -> Option<Engine> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
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
    let name = e.network().unique_name(ty);
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, format!("img.{ty}"), name),
    }];
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits)
        .unwrap_or_else(|err| panic!("img.{ty}: {err}"));
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

fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

struct Image(Vec<u8>);

impl Image {
    fn px(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        self.0[i..i + 4].try_into().unwrap()
    }
}

fn cook(e: &mut Engine, node: NodeId) -> Image {
    e.cook(&[node], 0.0).unwrap();
    assert_eq!(e.error(node), None);
    Image(read_rgba8(e.device(), e.queue(), e.result(node).expect("result")).unwrap())
}

fn close(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

#[track_caller]
fn assert_px(img: &Image, x: u32, y: u32, want: [u8; 4], tol: u8) {
    let got = img.px(x, y);
    assert!(
        close(got, want, tol),
        "pixel ({x},{y}) = {got:?}, want {want:?} ±{tol}"
    );
}

#[test]
fn constant() {
    let mut e = engine_or_skip!();
    let n = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 0.5, 0.0, 1.0]))],
    );
    let img = cook(&mut e, n);
    assert_px(&img, 0, 0, [255, 128, 0, 255], 1);
    assert_px(&img, W - 1, H - 1, [255, 128, 0, 255], 1);
}

#[test]
fn ramp_directions() {
    let mut e = engine_or_skip!();
    let n = add(&mut e, "ramp", &[]);
    let img = cook(&mut e, n);
    assert!(
        img.px(0, 8)[0] < 10 && img.px(W - 1, 8)[0] > 245,
        "horizontal: dark left, bright right"
    );
    set(&mut e, n, "type", json!("vertical"));
    let img = cook(&mut e, n);
    assert!(
        img.px(16, H - 1)[0] < 20 && img.px(16, 0)[0] > 235,
        "vertical: dark bottom, bright top"
    );
    set(&mut e, n, "type", json!("radial"));
    let img = cook(&mut e, n);
    assert!(
        img.px(16, 8)[0] < 30 && img.px(0, 0)[0] > 200,
        "radial: dark centre, bright corner"
    );
}

#[test]
fn noise_is_deterministic_varied_and_seeded() {
    let mut e = engine_or_skip!();
    let n = add(&mut e, "noise", &[]);
    let a = cook(&mut e, n).0;
    let lum: Vec<u8> = a.chunks(4).map(|p| p[0]).collect();
    let (min, max) = (lum.iter().min().unwrap(), lum.iter().max().unwrap());
    assert!(max - min > 40, "noise varies: {min}..{max}");
    assert!(
        a.chunks(4)
            .all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255),
        "monochrome, opaque"
    );
    let n2 = add(&mut e, "noise", &[]);
    assert_eq!(cook(&mut e, n2).0, a, "same parameters, same image");
    set(&mut e, n2, "seed", json!(7));
    assert_ne!(cook(&mut e, n2).0, a, "seed changes the pattern");
    set(&mut e, n2, "monochrome", json!(false));
    let c = cook(&mut e, n2).0;
    assert!(
        c.chunks(4).any(|p| p[0] != p[1]),
        "colour noise has independent channels"
    );
}

#[test]
fn level_adjusts() {
    let mut e = engine_or_skip!();
    let c = add(
        &mut e,
        "constant",
        &[("colour", json!([0.8, 0.4, 0.2, 1.0]))],
    );
    let l = add(&mut e, "level", &[("brightness", json!(0.5))]);
    wire(&mut e, c, l, "in");
    assert_px(&cook(&mut e, l), 3, 3, [102, 51, 26, 255], 2);
    set(&mut e, l, "brightness", json!(1.0));
    set(&mut e, l, "invert", json!(true));
    assert_px(&cook(&mut e, l), 3, 3, [51, 153, 204, 255], 2);
    set(&mut e, l, "invert", json!(false));
    set(&mut e, l, "opacity", json!(0.5));
    assert_px(&cook(&mut e, l), 3, 3, [204, 102, 51, 128], 2);
}

#[test]
fn hsv_rotates_hue() {
    let mut e = engine_or_skip!();
    let c = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    let h = add(&mut e, "hsv_adjust", &[("hue", json!(120.0))]);
    wire(&mut e, c, h, "in");
    assert_px(&cook(&mut e, h), 1, 1, [0, 255, 0, 255], 2);
    set(&mut e, h, "hue", json!(0.0));
    set(&mut e, h, "saturation", json!(0.0));
    assert_px(&cook(&mut e, h), 1, 1, [255, 255, 255, 255], 2);
}

#[test]
fn composite_operations() {
    let mut e = engine_or_skip!();
    let a = add(
        &mut e,
        "constant",
        &[("colour", json!([0.6, 0.2, 0.0, 1.0]))],
    );
    let b = add(
        &mut e,
        "constant",
        &[("colour", json!([0.2, 0.4, 1.0, 1.0]))],
    );
    let comp = add(&mut e, "composite", &[("operation", json!("add"))]);
    wire(&mut e, a, comp, "a");
    wire(&mut e, b, comp, "b");
    let cases = [
        ("add", [204, 153, 255, 255]),
        ("multiply", [31, 20, 0, 255]),
        ("difference", [102, 51, 255, 255]),
        ("maximum", [153, 102, 255, 255]),
        ("over", [153, 51, 0, 255]),
        ("average", [102, 77, 128, 255]),
    ];
    for (op, want) in cases {
        set(&mut e, comp, "operation", json!(op));
        let img = cook(&mut e, comp);
        assert!(
            close(img.px(2, 2), want, 2),
            "{op}: {:?} vs {want:?}",
            img.px(2, 2)
        );
    }
    set(&mut e, comp, "opacity", json!(0.0));
    assert_px(&cook(&mut e, comp), 2, 2, [51, 102, 255, 255], 2);
}

#[test]
fn threshold_splits_a_ramp() {
    let mut e = engine_or_skip!();
    let r = add(&mut e, "ramp", &[]);
    let t = add(&mut e, "threshold", &[("threshold", json!(0.5))]);
    wire(&mut e, r, t, "in");
    let img = cook(&mut e, t);
    assert_px(&img, 2, 4, [0, 0, 0, 255], 0);
    assert_px(&img, W - 3, 4, [255, 255, 255, 255], 0);
}

#[test]
fn mirror_tile_kaleidoscope() {
    let mut e = engine_or_skip!();
    let r = add(&mut e, "ramp", &[]);
    let m = add(&mut e, "mirror", &[]);
    wire(&mut e, r, m, "in");
    let img = cook(&mut e, m);
    for x in 0..W / 2 {
        assert!(
            close(img.px(x, 3), img.px(W - 1 - x, 3), 2),
            "mirror symmetric at x={x}"
        );
    }
    let t = add(&mut e, "tile", &[("repeat", json!([2.0, 1.0]))]);
    wire(&mut e, r, t, "in");
    let img = cook(&mut e, t);
    for x in 0..W / 2 {
        assert!(
            close(img.px(x, 3), img.px(x + W / 2, 3), 2),
            "tile repeats at x={x}"
        );
    }
    assert!(
        img.px(W / 2 - 1, 3)[0] > img.px(0, 3)[0] + 100,
        "each tile holds the whole ramp"
    );
    let k = add(&mut e, "kaleidoscope", &[]);
    wire(&mut e, r, k, "in");
    let img = cook(&mut e, k);
    assert!(img.0.chunks(4).all(|p| p[3] == 255));
}

#[test]
fn transform_translates_and_rotates() {
    let mut e = engine_or_skip!();
    let r = add(&mut e, "ramp", &[]);
    let t = add(
        &mut e,
        "transform",
        &[("translate", json!([0.5, 0.0])), ("extend", json!("zero"))],
    );
    wire(&mut e, r, t, "in");
    let img = cook(&mut e, t);
    assert_px(&img, 2, 8, [0, 0, 0, 0], 0);
    assert!(
        img.px(W / 2 + 1, 8)[0] < 20,
        "the ramp's dark start moved to the middle"
    );
    // Rotate 90° counter-clockwise: dark (left) goes to the bottom.
    set(&mut e, t, "translate", json!([0.0, 0.0]));
    set(&mut e, t, "rotate", json!(90.0));
    set(&mut e, t, "extend", json!("hold"));
    let img = cook(&mut e, t);
    assert!(
        img.px(16, H - 1)[0] < 90 && img.px(16, 0)[0] > 165,
        "ccw: dark bottom, bright top: {:?} {:?}",
        img.px(16, H - 1),
        img.px(16, 0)
    );
    // Scale 2× about the centre keeps the centre and pushes edges out.
    set(&mut e, t, "rotate", json!(0.0));
    set(&mut e, t, "scale", json!([2.0, 2.0]));
    let img = cook(&mut e, t);
    assert!(
        img.px(4, 8)[0] > 60 && img.px(W - 5, 8)[0] < 195,
        "zoomed in: {:?} {:?}",
        img.px(4, 8),
        img.px(W - 5, 8)
    );
}

#[test]
fn feedback_outputs_in_then_loop() {
    let mut e = engine_or_skip!();
    let c = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    let fb = add(&mut e, "feedback", &[]);
    let lvl = add(&mut e, "level", &[("brightness", json!(0.5))]);
    wire(&mut e, c, fb, "in");
    wire(&mut e, fb, lvl, "in");
    wire(&mut e, lvl, fb, "loop");
    // Frame 0: no loop frame yet, so fb = in (255) and level = 128.
    assert_px(&cook(&mut e, lvl), 1, 1, [128, 0, 0, 255], 2);
    // Each later frame halves again: fb = previous level output.
    assert_px(&cook(&mut e, lvl), 1, 1, [64, 0, 0, 255], 2);
    assert_px(&cook(&mut e, lvl), 1, 1, [32, 0, 0, 255], 2);
    set(&mut e, fb, "reset", json!(true));
    assert_px(&cook(&mut e, lvl), 1, 1, [128, 0, 0, 255], 2);
}

#[test]
fn blur_softens_edges_and_keeps_flat_areas() {
    let mut e = engine_or_skip!();
    let r = add(&mut e, "ramp", &[]);
    let t = add(&mut e, "threshold", &[]);
    let b = add(&mut e, "blur", &[("size", json!(6.0))]);
    wire(&mut e, r, t, "in");
    wire(&mut e, t, b, "in");
    let img = cook(&mut e, b);
    let edge = img.px(W / 2, 8)[0];
    assert!(edge > 40 && edge < 215, "edge is softened: {edge}");
    assert_px(&img, 0, 8, [0, 0, 0, 255], 3);
    assert_px(&img, W - 1, 8, [255, 255, 255, 255], 3);
}
