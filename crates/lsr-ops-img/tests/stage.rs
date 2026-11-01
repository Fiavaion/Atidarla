//! GPU tests for the Stage operators (M3): Layer cuts, fades and wipes
//! between clip slots, switches on beat boundaries of a scripted clock, and
//! Mixer composites layers through its crossfader.

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_interop::readback::read_rgba8;
use serde_json::{Value, json};

const W: u32 = 32;
const H: u32 = 16;

fn engine() -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_sig::library(&mut lib);
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
                    std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
                assert!(
                    !required,
                    "LSR_REQUIRE_GPU is set but there is no GPU adapter"
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
    e.edit("add", edits)
        .unwrap_or_else(|err| panic!("{ty}: {err}"));
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

/// Cooks `node` at `t` and reads pixel (x, 0).
fn cook_px(e: &mut Engine, node: NodeId, t: f64, x: u32) -> [u8; 4] {
    e.cook(&[node], t).unwrap();
    assert_eq!(e.error(node), None);
    let px = read_rgba8(e.device(), e.queue(), e.result(node).expect("result")).unwrap();
    let i = (x * 4) as usize;
    px[i..i + 4].try_into().unwrap()
}

#[track_caller]
fn near(got: [u8; 4], want: [u8; 4]) {
    assert!(
        got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 3),
        "got {got:?}, want {want:?}"
    );
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// A layer with a red clip in slot 0 and a blue one in slot 3.
fn red_blue_layer(e: &mut Engine, params: &[(&str, Value)]) -> NodeId {
    let red = add(
        e,
        "img.constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    let blue = add(
        e,
        "img.constant",
        &[("colour", json!([0.0, 0.0, 1.0, 1.0]))],
    );
    let layer = add(e, "img.layer", params);
    wire(e, red, layer, "clip0");
    wire(e, blue, layer, "clip3");
    layer
}

#[test]
fn layer_cuts_in_the_launch_frame() {
    let mut e = engine_or_skip!();
    let layer = red_blue_layer(&mut e, &[]);
    near(cook_px(&mut e, layer, 0.0, 0), [0, 0, 0, 0]);
    assert_eq!(e.status(layer), Some("playing none"));
    set(&mut e, layer, "target", json!(0));
    near(cook_px(&mut e, layer, 0.1, 0), RED);
    set(&mut e, layer, "target", json!(3));
    near(cook_px(&mut e, layer, 0.2, 0), BLUE);
    assert_eq!(e.status(layer), Some("playing 3"));
    set(&mut e, layer, "opacity", json!(0.5));
    near(cook_px(&mut e, layer, 0.3, 0), [0, 0, 255, 128]);
    set(&mut e, layer, "bypass", json!(true));
    near(cook_px(&mut e, layer, 0.4, 0), [0, 0, 0, 0]);
}

#[test]
fn layer_fades_and_wipes_over_time() {
    let mut e = engine_or_skip!();
    let layer = red_blue_layer(
        &mut e,
        &[
            ("target", json!(0)),
            ("transition", json!("fade")),
            ("transition_unit", json!("seconds")),
            ("transition_length", json!(1.0)),
        ],
    );
    near(cook_px(&mut e, layer, 0.0, 0), RED);
    set(&mut e, layer, "target", json!(3));
    near(cook_px(&mut e, layer, 1.0, 0), RED);
    // Half-way, with `over` and opaque clips: ¼ outgoing, ¾ incoming.
    near(cook_px(&mut e, layer, 1.5, 0), [64, 0, 191, 255]);
    near(cook_px(&mut e, layer, 2.0, 0), BLUE);
    assert!(!e.pending(), "the fade is over: no more cooks requested");

    set(&mut e, layer, "transition", json!("wipe"));
    set(&mut e, layer, "target", json!(0));
    cook_px(&mut e, layer, 3.0, 0);
    // Half-way the left half shows the incoming clip.
    near(cook_px(&mut e, layer, 3.5, 2), RED);
    near(cook_px(&mut e, layer, 3.5, W - 2), BLUE);
}

#[test]
fn quantised_launch_switches_on_the_clock_boundary() {
    let mut e = engine_or_skip!();
    let clock = add(
        &mut e,
        "sig.constant",
        &[
            ("name1", json!("beat")),
            ("value1", json!(0.0)),
            ("name2", json!("beat_phase")),
            ("value2", json!(0.25)),
            ("name3", json!("bar")),
            ("value3", json!(0.0)),
        ],
    );
    let layer = red_blue_layer(&mut e, &[("target", json!(0)), ("quantise", json!("beat"))]);
    wire(&mut e, clock, layer, "clock");
    near(cook_px(&mut e, layer, 0.0, 0), RED);
    set(&mut e, layer, "target", json!(3));
    near(cook_px(&mut e, layer, 0.1, 0), RED);
    assert_eq!(e.status(layer), Some("playing 0 · queued 3"));
    set(&mut e, clock, "value2", json!(0.9));
    near(cook_px(&mut e, layer, 0.2, 0), RED);
    // The next beat starts: the switch happens in that frame.
    set(&mut e, clock, "value1", json!(1.0));
    set(&mut e, clock, "value2", json!(0.0));
    near(cook_px(&mut e, layer, 0.3, 0), BLUE);
    assert_eq!(e.status(layer), Some("playing 3"));

    // Bar quantise ignores beats within the bar.
    set(&mut e, layer, "quantise", json!("bar"));
    set(&mut e, layer, "target", json!(0));
    for beat in [2.0, 3.0] {
        set(&mut e, clock, "value1", json!(beat));
        near(cook_px(&mut e, layer, 0.4, 0), BLUE);
    }
    set(&mut e, clock, "value1", json!(4.0));
    set(&mut e, clock, "value3", json!(1.0));
    near(cook_px(&mut e, layer, 0.5, 0), RED);
}

#[test]
fn mixer_composites_layers_through_the_crossfader() {
    let mut e = engine_or_skip!();
    let red = add(
        &mut e,
        "img.constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    let blue = add(
        &mut e,
        "img.constant",
        &[("colour", json!([0.0, 0.0, 1.0, 1.0]))],
    );
    let mixer = add(
        &mut e,
        "img.mixer",
        &[
            ("group0", json!("a")),
            ("group1", json!("b")),
            ("crossfader", json!(0.0)),
        ],
    );
    wire(&mut e, red, mixer, "layer0");
    wire(&mut e, blue, mixer, "layer1");
    near(cook_px(&mut e, mixer, 0.0, 0), RED);
    set(&mut e, mixer, "crossfader", json!(1.0));
    near(cook_px(&mut e, mixer, 0.0, 0), BLUE);
    // Centre: both at full, blue over red.
    set(&mut e, mixer, "crossfader", json!(0.5));
    near(cook_px(&mut e, mixer, 0.0, 0), BLUE);
    set(&mut e, mixer, "blend1", json!("add"));
    near(cook_px(&mut e, mixer, 0.0, 0), [255, 0, 255, 255]);
    set(&mut e, mixer, "opacity1", json!(0.5));
    near(cook_px(&mut e, mixer, 0.0, 0), [255, 0, 128, 255]);
    set(&mut e, mixer, "master", json!(0.5));
    near(cook_px(&mut e, mixer, 0.0, 0), [128, 0, 64, 255]);
}

/// A layer cooks only the clips it shows: a clip in another slot does not
/// cook while it waits, even when an edit dirties it, and shows its current
/// state in the frame it is launched.
#[test]
fn only_the_playing_clip_cooks() {
    let mut e = engine_or_skip!();
    let red = add(
        &mut e,
        "img.constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    let waiting = add(
        &mut e,
        "img.constant",
        &[("colour", json!([0.0, 0.0, 1.0, 1.0]))],
    );
    let layer = add(&mut e, "img.layer", &[("target", json!(0))]);
    wire(&mut e, red, layer, "clip0");
    wire(&mut e, waiting, layer, "clip5");
    near(cook_px(&mut e, layer, 0.0, 0), RED);
    // The first frame cooks every clip once: the layer's operator is
    // created during that cook, after the plan.
    let cooks = |e: &Engine, n| e.stat(n).map_or(0, |s| s.cooks);
    let (before, red_before) = (cooks(&e, waiting), cooks(&e, red));
    for (i, c) in [[0.0, 1.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]]
        .iter()
        .enumerate()
    {
        set(&mut e, waiting, "colour", json!(c));
        near(cook_px(&mut e, layer, 0.1 * (i + 1) as f64, 0), RED);
    }
    assert_eq!(
        cooks(&e, waiting),
        before,
        "edits to a waiting clip do not cook it"
    );
    set(&mut e, layer, "target", json!(5));
    near(cook_px(&mut e, layer, 0.5, 0), [255, 255, 255, 255]);
    assert_eq!(cooks(&e, waiting), before + 1, "cooked in the launch frame");
    set(&mut e, red, "colour", json!([0.5, 0.0, 0.0, 1.0]));
    cook_px(&mut e, layer, 0.6, 0);
    assert_eq!(
        cooks(&e, red),
        red_before,
        "the clip that stopped does not cook"
    );
}

/// The clip in `layer`'s slot 3 (blue in [`red_blue_layer`]).
fn slot3_clip(e: &Engine, layer: NodeId) -> NodeId {
    e.network()
        .inputs_of(layer)
        .find(|(n, _)| *n == "clip3")
        .map(|(_, s)| s.node)
        .unwrap()
}

/// A `target` bound to a channel is only known once the channel cooks, so
/// the clip it switches to must cook in the switch frame (not show the
/// frame it last cooked, or nothing).
#[test]
fn bound_target_shows_the_current_clip_in_the_switch_frame() {
    use lsr_core::graph::ChannelRef;
    let mut e = engine_or_skip!();
    let ctl = add(
        &mut e,
        "sig.constant",
        &[("name1", json!("slot")), ("value1", json!(0.0))],
    );
    let layer = red_blue_layer(&mut e, &[("target", json!(0))]);
    e.edit(
        "bind",
        [Edit::SetParam {
            node: layer,
            param: "target".into(),
            value: Some(Param {
                value: json!(0),
                expr: None,
                bind: Some(ChannelRef {
                    node: ctl,
                    channel: "slot".into(),
                }),
            }),
        }],
    )
    .unwrap();
    near(cook_px(&mut e, layer, 0.0, 0), RED);
    near(cook_px(&mut e, layer, 0.05, 0), RED);
    // Slot 3's clip turns green while it is not shown.
    let idle = slot3_clip(&e, layer);
    set(&mut e, idle, "colour", json!([0.0, 1.0, 0.0, 1.0]));
    near(cook_px(&mut e, layer, 0.1, 0), RED);
    set(&mut e, ctl, "value1", json!(3.0));
    near(cook_px(&mut e, layer, 0.2, 0), [0, 255, 0, 255]);
}

/// The same for a `target` driven by an expression of time.
#[test]
fn expression_target_shows_the_current_clip_in_the_switch_frame() {
    let mut e = engine_or_skip!();
    let layer = red_blue_layer(&mut e, &[("target", json!(0))]);
    e.edit(
        "expr",
        [Edit::SetParam {
            node: layer,
            param: "target".into(),
            value: Some(Param {
                value: json!(0),
                expr: Some("0 if time < 1 else 3".into()),
                bind: None,
            }),
        }],
    )
    .unwrap();
    near(cook_px(&mut e, layer, 0.0, 0), RED);
    let idle = slot3_clip(&e, layer);
    set(&mut e, idle, "colour", json!([0.0, 1.0, 0.0, 1.0]));
    near(cook_px(&mut e, layer, 0.5, 0), RED);
    near(cook_px(&mut e, layer, 1.5, 0), [0, 255, 0, 255]);
}
