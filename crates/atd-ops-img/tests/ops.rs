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
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
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
    let full = if ty.contains('.') {
        ty.to_owned()
    } else {
        format!("img.{ty}")
    };
    let id = e.alloc_id();
    let name = e.network().unique_name(full.split('.').nth(1).unwrap());
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, full.clone(), name),
    }];
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits)
        .unwrap_or_else(|err| panic!("{full}: {err}"));
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

#[test]
fn sig_channels_drive_img_parameters() {
    let mut e = engine_or_skip!();
    let white = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let level = add(&mut e, "level", &[]);
    wire(&mut e, white, level, "in");
    let k = add(&mut e, "sig.constant", &[("value1", json!(0.5))]);
    e.edit(
        "bind",
        [Edit::SetParam {
            node: level,
            param: "brightness".into(),
            value: Some(Param::bound(json!(1.0), k, "chan1")),
        }],
    )
    .unwrap();
    assert_px(&cook(&mut e, level), 1, 1, [128, 128, 128, 255], 2);
    // Changing the channel source re-cooks the bound IMG node.
    set(&mut e, k, "value1", json!(0.25));
    assert_px(&cook(&mut e, level), 1, 1, [64, 64, 64, 255], 2);

    // A time-dependent LFO animates it: square wave between 0 and 1.
    let lfo = add(
        &mut e,
        "sig.lfo",
        &[
            ("waveform", json!("square")),
            ("amplitude", json!(0.5)),
            ("offset", json!(0.5)),
        ],
    );
    e.edit(
        "bind",
        [Edit::SetParam {
            node: level,
            param: "brightness".into(),
            value: Some(Param::bound(json!(1.0), lfo, "lfo")),
        }],
    )
    .unwrap();
    let at = |e: &mut Engine, t: f64| {
        e.cook(&[level], t).unwrap();
        Image(read_rgba8(e.device(), e.queue(), e.result(level).unwrap()).unwrap()).px(1, 1)
    };
    assert!(close(at(&mut e, 0.1), [255, 255, 255, 255], 1));
    assert!(close(at(&mut e, 0.6), [0, 0, 0, 255], 1));
    assert!(close(at(&mut e, 1.2), [255, 255, 255, 255], 1));
    assert!(
        e.channels(lfo).is_some(),
        "the engine exposes channel results"
    );
}

/// The M1 acceptance pattern (audio-reactive patch) end to end: a WAV plays
/// through Audio File In → Spectrum, whose `level` channel drives an IMG
/// parameter rendered on the GPU.
#[test]
fn audio_drives_img_through_spectrum() {
    let mut e = engine_or_skip!();
    let path = std::env::temp_dir().join(format!("atd-reactive-{}.wav", std::process::id()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..48_000 {
        let s = (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin() * 0.5;
        w.write_sample((s * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();

    let audio = add(
        &mut e,
        "sig.audio_file_in",
        &[("file", json!(path.to_string_lossy()))],
    );
    let spectrum = add(&mut e, "sig.spectrum", &[("smoothing", json!(0.0))]);
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(audio, "out"), Endpoint::new(spectrum, "in")),
        }],
    )
    .unwrap();
    let white = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let level = add(&mut e, "level", &[]);
    wire(&mut e, white, level, "in");
    e.edit(
        "bind",
        [Edit::SetParam {
            node: level,
            param: "brightness".into(),
            value: Some(Param::bound(json!(0.0), spectrum, "level")),
        }],
    )
    .unwrap();

    // Silence until the file has decoded (off the render thread), then the
    // tone's RMS (0.5 / sqrt 2 ≈ 0.354) sets the brightness.
    let mut t = 0.0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let px = loop {
        t += 1.0 / 60.0;
        e.cook(&[level], t).unwrap();
        let p =
            Image(read_rgba8(e.device(), e.queue(), e.result(level).unwrap()).unwrap()).px(1, 1);
        // Wait until the 1024-sample window is full of tone.
        if e.channels(spectrum)
            .and_then(|c| c.value("level"))
            .is_some_and(|l| l > 0.35)
        {
            break p;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audio reached the image"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(
        close(px, [90, 90, 90, 255], 3),
        "brightness follows the audio level: {px:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn shape_draws_circle_rectangle_polygon() {
    let mut e = engine_or_skip!();
    let s = add(
        &mut e,
        "shape",
        &[("size", json!([0.5, 0.5])), ("softness", json!(0.0))],
    );
    let img = cook(&mut e, s);
    assert_px(&img, W / 2, H / 2, [255, 255, 255, 255], 0);
    assert_px(&img, 0, 0, [0, 0, 0, 0], 0);
    set(&mut e, s, "shape", json!("rectangle"));
    let img = cook(&mut e, s);
    assert_px(&img, W / 2, H / 2, [255, 255, 255, 255], 0);
    assert_px(&img, 0, H / 2, [0, 0, 0, 0], 0);
    set(&mut e, s, "shape", json!("polygon"));
    assert_px(&cook(&mut e, s), W / 2, H / 2, [255, 255, 255, 255], 0);
}

#[test]
fn blend_channel_mix_lookup() {
    let mut e = engine_or_skip!();
    let red = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    let blue = add(
        &mut e,
        "constant",
        &[("colour", json!([0.0, 0.0, 1.0, 1.0]))],
    );
    let b = add(&mut e, "blend", &[]);
    wire(&mut e, red, b, "a");
    wire(&mut e, blue, b, "b");
    assert_px(&cook(&mut e, b), 1, 1, [128, 0, 128, 255], 2);

    let c = add(
        &mut e,
        "constant",
        &[("colour", json!([1.0, 0.5, 0.25, 1.0]))],
    );
    let m = add(
        &mut e,
        "channel_mix",
        &[
            ("red", json!([0.0, 1.0, 0.0, 0.0])),
            ("blue", json!([1.0, 0.0, 0.0, 0.0])),
        ],
    );
    wire(&mut e, c, m, "in");
    assert_px(&cook(&mut e, m), 1, 1, [128, 128, 255, 255], 2);

    let grey = add(
        &mut e,
        "constant",
        &[("colour", json!([0.25, 0.25, 0.25, 1.0]))],
    );
    let lut = add(&mut e, "ramp", &[("colour_b", json!([1.0, 0.0, 0.0, 1.0]))]);
    let l = add(&mut e, "lookup", &[]);
    wire(&mut e, grey, l, "in");
    wire(&mut e, lut, l, "lut");
    assert_px(&cook(&mut e, l), 1, 1, [64, 0, 0, 255], 6);
}

#[test]
fn remap_and_displace() {
    let mut e = engine_or_skip!();
    let ramp = add(&mut e, "ramp", &[]);
    let map = add(
        &mut e,
        "constant",
        &[("colour", json!([0.75, 0.5, 0.0, 1.0]))],
    );
    let r = add(&mut e, "remap", &[]);
    wire(&mut e, ramp, r, "in");
    wire(&mut e, map, r, "map");
    let img = cook(&mut e, r);
    assert_px(&img, 0, 0, img.px(W - 1, H - 1), 0);
    assert!(
        close(img.px(3, 3), [191, 191, 191, 255], 8),
        "{:?}",
        img.px(3, 3)
    );

    let base = cook(&mut e, ramp);
    let by = add(
        &mut e,
        "constant",
        &[("colour", json!([0.75, 0.5, 0.0, 1.0]))],
    );
    let d = add(&mut e, "displace", &[("amount", json!(0.4))]);
    wire(&mut e, ramp, d, "in");
    wire(&mut e, by, d, "by");
    let img = cook(&mut e, d);
    let shift = img.px(10, 5)[0] as i32 - base.px(10, 5)[0] as i32;
    assert!(
        (shift - 26).abs() <= 3,
        "displaced by 0.1 of the width: {shift}"
    );
}

#[test]
fn keys() {
    let mut e = engine_or_skip!();
    let green = add(
        &mut e,
        "constant",
        &[("colour", json!([0.0, 1.0, 0.0, 1.0]))],
    );
    let ck = add(&mut e, "chroma_key", &[]);
    wire(&mut e, green, ck, "in");
    assert_eq!(cook(&mut e, ck).px(1, 1)[3], 0, "green is keyed");
    set(&mut e, green, "colour", json!([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(cook(&mut e, ck).px(1, 1)[3], 255, "red is kept");

    let grey = add(
        &mut e,
        "constant",
        &[("colour", json!([0.25, 0.25, 0.25, 1.0]))],
    );
    let lk = add(&mut e, "luma_key", &[]);
    wire(&mut e, grey, lk, "in");
    assert_eq!(cook(&mut e, lk).px(1, 1)[3], 255, "inside the range");
    set(&mut e, lk, "low", json!(0.5));
    set(&mut e, lk, "high", json!(1.0));
    assert_eq!(cook(&mut e, lk).px(1, 1)[3], 0, "outside the range");
}

#[test]
fn edge_normal_slope() {
    let mut e = engine_or_skip!();
    let ramp = add(&mut e, "ramp", &[]);
    let t = add(&mut e, "threshold", &[]);
    wire(&mut e, ramp, t, "in");
    let edge = add(&mut e, "edge", &[]);
    wire(&mut e, t, edge, "in");
    let img = cook(&mut e, edge);
    assert!(
        img.px(W / 2, 8)[0] > 200 || img.px(W / 2 - 1, 8)[0] > 200,
        "edge at the step"
    );
    assert_px(&img, 2, 8, [0, 0, 0, 255], 0);

    let flat = add(
        &mut e,
        "constant",
        &[("colour", json!([0.5, 0.5, 0.5, 1.0]))],
    );
    let n = add(&mut e, "normal_map", &[]);
    wire(&mut e, flat, n, "in");
    assert_px(&cook(&mut e, n), 4, 4, [128, 128, 255, 255], 1);
    let s = add(&mut e, "slope", &[]);
    wire(&mut e, flat, s, "in");
    assert_px(&cook(&mut e, s), 4, 4, [128, 128, 128, 255], 1);
    wire(&mut e, ramp, s, "in");
    assert!(
        cook(&mut e, s).px(10, 8)[0] > 128,
        "a rising ramp has positive x slope"
    );
}

#[test]
fn bloom_glows_beyond_bright_areas() {
    let mut e = engine_or_skip!();
    let dot = add(
        &mut e,
        "shape",
        &[
            ("size", json!([0.2, 0.2])),
            ("background", json!([0.0, 0.0, 0.0, 1.0])),
            ("softness", json!(0.0)),
        ],
    );
    let b = add(
        &mut e,
        "bloom",
        &[
            ("threshold", json!(0.5)),
            ("size", json!(6.0)),
            ("intensity", json!(4.0)),
        ],
    );
    wire(&mut e, dot, b, "in");
    let without = cook(&mut e, dot);
    let with = cook(&mut e, b);
    let x = W / 2 + 3;
    assert_eq!(without.px(x, H / 2)[0], 0, "outside the dot is black");
    assert!(
        with.px(x, H / 2)[0] > 10,
        "bloom spills outside: {:?}",
        with.px(x, H / 2)
    );
}

#[test]
fn crop_and_switch() {
    let mut e = engine_or_skip!();
    let ramp = add(&mut e, "ramp", &[]);
    let c = add(
        &mut e,
        "crop",
        &[("min", json!([0.5, 0.0])), ("max", json!([1.0, 1.0]))],
    );
    wire(&mut e, ramp, c, "in");
    let img = cook(&mut e, c);
    assert!(
        img.px(0, 8)[0].abs_diff(128) <= 6,
        "left edge now starts mid-ramp: {:?}",
        img.px(0, 8)
    );

    let colours = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 1.0, 1.0],
    ];
    let sw = add(&mut e, "switch", &[("index", json!(2.0))]);
    for (i, col) in colours.iter().enumerate() {
        let k = add(&mut e, "constant", &[("colour", json!(col))]);
        wire(&mut e, k, sw, &format!("in{i}"));
    }
    assert_px(&cook(&mut e, sw), 1, 1, [0, 0, 255, 255], 0);
    set(&mut e, sw, "index", json!(0.5));
    set(&mut e, sw, "blend", json!(true));
    assert_px(&cook(&mut e, sw), 1, 1, [128, 128, 0, 255], 2);
}

#[test]
fn cache_delays_frames() {
    let mut e = engine_or_skip!();
    let k = add(&mut e, "constant", &[]);
    let c = add(&mut e, "cache", &[("delay", json!(2))]);
    wire(&mut e, k, c, "in");
    let seq = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
    let mut seen = Vec::new();
    for (t, col) in seq.iter().enumerate() {
        let f = col.map(|v| f64::from(v) / 255.0);
        set(&mut e, k, "colour", json!([f[0], f[1], f[2], 1.0]));
        e.cook(&[c], t as f64).unwrap();
        let px = Image(read_rgba8(e.device(), e.queue(), e.result(c).unwrap()).unwrap()).px(1, 1);
        seen.push([px[0], px[1], px[2]]);
    }
    // Until two frames of history exist, the oldest frame is shown.
    assert_eq!(seen, [seq[0], seq[0], seq[0], seq[1]]);
}

#[test]
fn ripple_spreads_waves_and_resets() {
    let mut e = engine_or_skip!();
    let drop = add(
        &mut e,
        "shape",
        &[("size", json!([0.15, 0.15])), ("softness", json!(0.0))],
    );
    let r = add(&mut e, "ripple", &[]);
    wire(&mut e, drop, r, "drive");
    let mut img = Image(Vec::new());
    for t in 0..6 {
        e.cook(&[r], t as f64).unwrap();
        img = Image(read_rgba8(e.device(), e.queue(), e.result(r).unwrap()).unwrap());
    }
    let lums: Vec<u8> = img.0.chunks(4).map(|p| p[0]).collect();
    assert!(
        lums.iter().max().unwrap() - lums.iter().min().unwrap() > 20,
        "waves vary the surface"
    );
    set(&mut e, r, "reset", json!(true));
    e.cook(&[r], 7.0).unwrap();
    // With the drive still on, only the drive area moves right after a reset.
    let img = Image(read_rgba8(e.device(), e.queue(), e.result(r).unwrap()).unwrap());
    assert_px(&img, 0, 0, [128, 128, 128, 255], 1);
}

#[test]
fn reaction_diffusion_grows_from_the_seed() {
    let mut e = engine_or_skip!();
    let rd = add(&mut e, "reaction_diffusion", &[("steps", json!(32))]);
    let first = cook(&mut e, rd);
    assert!(
        first.px(W / 2, H / 2)[0] > 200,
        "the centre seed shows colour B"
    );
    assert_px(&first, 0, 0, [0, 0, 0, 255], 1);
    let mut img = first;
    for t in 1..20 {
        e.cook(&[rd], t as f64).unwrap();
        img = Image(read_rgba8(e.device(), e.queue(), e.result(rd).unwrap()).unwrap());
    }
    let lit = img.0.chunks(4).filter(|p| p[0] > 60).count();
    assert!(
        lit > 4,
        "pattern present after 20 frames ({lit} lit pixels)"
    );
}

#[test]
fn channel_image_draws_channels_as_rows_or_columns() {
    let mut e = engine_or_skip!();
    let sig = add(
        &mut e,
        "sig.constant",
        &[
            ("name1", json!("a")),
            ("value1", json!(0.25)),
            ("name2", json!("b")),
            ("value2", json!(0.75)),
        ],
    );
    let img = add(&mut e, "channel_image", &[("interpolate", json!(false))]);
    wire(&mut e, sig, img, "sig");

    // Samples layout: one row per channel, channel 0 at the bottom.
    let out = cook(&mut e, img);
    assert_px(&out, 3, H - 1, [64, 64, 64, 255], 2);
    assert_px(&out, W - 1, 0, [191, 191, 191, 255], 2);

    // Channels layout: channels along x.
    set(&mut e, img, "layout", json!("channels"));
    let out = cook(&mut e, img);
    assert_px(&out, 0, 5, [64, 64, 64, 255], 2);
    assert_px(&out, W - 1, 5, [191, 191, 191, 255], 2);

    // Interpolated: the ends are exact and the middle blends.
    set(&mut e, img, "interpolate", json!(true));
    let out = cook(&mut e, img);
    assert_px(&out, 0, 5, [66, 66, 66, 255], 4);
    assert_px(&out, W / 2, 5, [128, 128, 128, 255], 6);
    assert_px(&out, W - 1, 5, [189, 189, 189, 255], 4);

    // Scale and offset map −1…1 to 0…1; with no input the image is black.
    set(&mut e, img, "scale", json!(0.5));
    set(&mut e, img, "offset", json!(0.5));
    set(&mut e, img, "interpolate", json!(false));
    assert_px(&cook(&mut e, img), 0, 5, [159, 159, 159, 255], 2);
    e.edit(
        "unwire",
        [Edit::Disconnect {
            wire: Wire::new(Endpoint::new(sig, "out"), Endpoint::new(img, "sig")),
        }],
    )
    .unwrap();
    assert_px(&cook(&mut e, img), 0, 5, [0, 0, 0, 255], 0);
}

#[test]
fn projection_maps_directions() {
    let mut e = engine_or_skip!();
    // A horizontal ramp as an equirect panorama: value = longitude / 360° + ½.
    let ramp = add(&mut e, "ramp", &[]);
    let proj = add(
        &mut e,
        "projection",
        &[("from", json!("equirect")), ("to", json!("perspective"))],
    );
    wire(&mut e, ramp, proj, "in");
    let img = cook(&mut e, proj);
    let mid = img.px(W / 2, H / 2)[0];
    assert!(
        mid.abs_diff(128) <= 6,
        "looking forward sees longitude 0: {mid}"
    );
    assert!(img.px(2, H / 2)[0] < mid && img.px(W - 3, H / 2)[0] > mid);
    // Yaw 90°: looking right (+x) sees longitude 90° (ramp ¾).
    set(&mut e, proj, "rotate", json!([90.0, 0.0, 0.0]));
    let img = cook(&mut e, proj);
    assert!(
        img.px(W / 2, H / 2)[0].abs_diff(191) <= 6,
        "{:?}",
        img.px(W / 2, H / 2)
    );

    // Fisheye output: transparent outside the circle.
    set(&mut e, proj, "rotate", json!([0.0, 0.0, 0.0]));
    set(&mut e, proj, "to", json!("fisheye"));
    set(&mut e, proj, "fov", json!(180.0));
    let img = cook(&mut e, proj);
    assert_eq!(img.px(0, 0), [0, 0, 0, 0]);
    assert!(img.px(W / 2, H / 2)[0].abs_diff(128) <= 6);

    // Equirect → cube cross → equirect keeps the panorama (coarsely at 32×16).
    set(&mut e, proj, "to", json!("cube"));
    let back = add(
        &mut e,
        "projection",
        &[("from", json!("cube")), ("to", json!("equirect"))],
    );
    wire(&mut e, proj, back, "in");
    let original = cook(&mut e, ramp);
    let round = cook(&mut e, back);
    for x in [6, 10, 16, 22, 26] {
        let (a, b) = (original.px(x, H / 2)[0], round.px(x, H / 2)[0]);
        assert!(a.abs_diff(b) <= 24, "x {x}: {a} vs {b}");
    }
}

#[test]
fn fluid_carries_dye_upward_and_resets() {
    let mut e = engine_or_skip!();
    // A radius that covers several cells of the 32×16 grid.
    let f = add(
        &mut e,
        "fluid",
        &[("detail", json!(1.0)), ("emitter_radius", json!(0.2))],
    );
    // Two seconds of 60 fps.
    let mut img = None;
    for frame in 0..120 {
        e.cook(&[f], frame as f64 / 60.0).unwrap();
        assert_eq!(e.error(f), None);
        if frame == 119 {
            img = Some(Image(
                read_rgba8(e.device(), e.queue(), e.result(f).unwrap()).unwrap(),
            ));
        }
    }
    let img = img.unwrap();
    // Dye-weighted mean row (0 = top); the emitter sits at 20% from the bottom.
    let (mut sum, mut weighted) = (0.0f64, 0.0f64);
    for y in 0..H {
        for x in 0..W {
            let r = f64::from(img.px(x, y)[0]);
            sum += r;
            weighted += r * f64::from(y);
        }
    }
    assert!(sum > 1000.0, "dye present: {sum}");
    let mean_row = weighted / sum / f64::from(H - 1);
    assert!(
        mean_row < 0.75,
        "dye rose above the emitter (row {mean_row:.2})"
    );

    // Reset clears; without the emitter nothing appears.
    set(&mut e, f, "reset", json!(true));
    set(&mut e, f, "emitter", json!(false));
    e.cook(&[f], 2.1).unwrap();
    set(&mut e, f, "reset", json!(false));
    for frame in 0..10 {
        e.cook(&[f], 2.2 + frame as f64 / 60.0).unwrap();
    }
    let img = cook(&mut e, f);
    assert!(
        (0..W * H).all(|i| img.px(i % W, i / W)[0] == 0),
        "empty fluid stays black"
    );
}

#[test]
fn time_machine_reaches_back_in_time() {
    let mut e = engine_or_skip!();
    let c = add(
        &mut e,
        "constant",
        &[("colour", json!([0.0, 0.0, 0.0, 1.0]))],
    );
    let tm = add(&mut e, "time_machine", &[("frames", json!(10))]);
    wire(&mut e, c, tm, "in");
    // Frame k shows red k/20.
    let mut img = None;
    for k in 0..15 {
        set(&mut e, c, "colour", json!([k as f64 / 20.0, 0.0, 0.0, 1.0]));
        img = Some(cook(&mut e, tm));
    }
    let red = |v: f64| (v * 255.0).round() as u8;
    assert!(
        img.unwrap().px(5, 5)[0].abs_diff(red(14.0 / 20.0)) <= 1,
        "now"
    );
    // Offset 1: the oldest kept frame, nine frames back.
    set(&mut e, tm, "offset", json!(1.0));
    set(&mut e, c, "colour", json!([15.0 / 20.0, 0.0, 0.0, 1.0]));
    let img = cook(&mut e, tm);
    assert!(
        img.px(5, 5)[0].abs_diff(red(6.0 / 20.0)) <= 1,
        "{:?}",
        img.px(5, 5)
    );

    // A ramp as the time map: left is now, right is long ago.
    set(&mut e, tm, "offset", json!(0.0));
    let ramp = add(&mut e, "ramp", &[]);
    wire(&mut e, ramp, tm, "time");
    set(&mut e, c, "colour", json!([16.0 / 20.0, 0.0, 0.0, 1.0]));
    let img = cook(&mut e, tm);
    let (left, right) = (img.px(0, 5)[0], img.px(W - 1, 5)[0]);
    assert!(left.abs_diff(red(16.0 / 20.0)) <= 3, "left {left}");
    assert!(right.abs_diff(red(7.0 / 20.0)) <= 8, "right {right}");
    assert!(
        (1..W).all(|x| img.px(x, 5)[0] <= img.px(x - 1, 5)[0]),
        "older to the right"
    );

    // A fresh history starts filled with the first frame, not black.
    set(&mut e, tm, "frames", json!(20));
    let img = cook(&mut e, tm);
    assert!(img.px(W - 1, 5)[0].abs_diff(red(16.0 / 20.0)) <= 1);
}

#[test]
fn resolution_sets_size_and_downstream_follows() {
    let mut e = engine_or_skip!();
    let ramp = add(&mut e, "ramp", &[]);
    let res = add(&mut e, "resolution", &[("fraction", json!(0.5))]);
    let level = add(&mut e, "level", &[]);
    let cache = add(&mut e, "cache", &[("delay", json!(1))]);
    wire(&mut e, ramp, res, "in");
    wire(&mut e, res, level, "in");
    wire(&mut e, level, cache, "in");
    for t in 0..3 {
        e.cook(&[cache], f64::from(t)).unwrap();
    }
    let size = |e: &Engine, n: NodeId| {
        let t = e.result(n).unwrap();
        (t.width(), t.height())
    };
    assert_eq!(size(&e, ramp), (W, H), "generators use the project size");
    assert_eq!(size(&e, res), (W / 2, H / 2));
    assert_eq!(
        size(&e, level),
        (W / 2, H / 2),
        "downstream follows its input"
    );
    assert_eq!(
        size(&e, cache),
        (W / 2, H / 2),
        "copies work at the new size"
    );
    assert_eq!(e.error(cache), None);
    // The resampled ramp still rises left to right.
    let px = read_rgba8(e.device(), e.queue(), e.result(level).unwrap()).unwrap();
    let w = (W / 2) as usize;
    assert!(px[0] < px[(w / 2) * 4] && px[(w / 2) * 4] < px[(w - 1) * 4]);

    set(&mut e, res, "mode", json!("pixels"));
    set(&mut e, res, "width", json!(10));
    set(&mut e, res, "height", json!(4));
    e.cook(&[cache], 4.0).unwrap();
    assert_eq!(size(&e, level), (10, 4));
}
