//! GPU tests for ISF import (FR-K6): coordinate conventions, parameters,
//! multi-pass targets, persistent feedback and vertex shaders. The shaders
//! here are written for these tests.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use atd_ops_img::isf::{IsfDef, register};
use serde_json::{Value, json};

const W: u32 = 32;
const H: u32 = 16;

fn engine(defs: Vec<IsfDef>) -> Option<Engine> {
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
    let n = defs.len();
    assert_eq!(register(defs, &mut reg, &mut lib).len(), n);
    let config = EngineConfig {
        width: W,
        height: H,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

macro_rules! engine_or_skip {
    ($defs:expr) => {
        match engine($defs) {
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

fn def(stem: &str, fs: &str, vs: Option<&str>) -> IsfDef {
    IsfDef::new(stem, fs, vs).unwrap_or_else(|e| panic!("{stem}: {e}"))
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name("n");
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty.to_owned(), name),
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

struct Image(Vec<u8>);

impl Image {
    fn px(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        self.0[i..i + 4].try_into().unwrap()
    }
}

fn cook_at(e: &mut Engine, node: NodeId, time: f64) -> Image {
    e.cook(&[node], time).unwrap();
    // Shaders compile on a worker thread; the node is black until then.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while e.pending() {
        assert!(std::time::Instant::now() < deadline, "shader compiled");
        std::thread::sleep(std::time::Duration::from_millis(2));
        e.cook(&[node], time).unwrap();
    }
    assert_eq!(e.error(node), None);
    Image(read_rgba8(e.device(), e.queue(), e.result(node).expect("result")).unwrap())
}

#[track_caller]
fn assert_px(img: &Image, x: u32, y: u32, want: [u8; 4], tol: u8) {
    let got = img.px(x, y);
    assert!(
        got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= tol),
        "pixel ({x},{y}) = {got:?}, want {want:?} ±{tol}"
    );
}

const COORDS: &str = r#"/*{
    "DESCRIPTION": "Writes the normalised coordinate and the flipped fragment coordinate.",
    "INPUTS": []
}*/
void main() {
    vec2 a = isf_FragNormCoord;
    vec2 b = gl_FragCoord.xy / RENDERSIZE;
    gl_FragColor = vec4(a, b.y, 1.0);
}
"#;

#[test]
fn coordinates_follow_isf_bottom_left_origin() {
    let mut e = engine_or_skip!(vec![def("Coords", COORDS, None)]);
    let n = add(&mut e, "img.isf_coords", &[]);
    let img = cook_at(&mut e, n, 0.0);
    // Top-left texel: x ≈ 0, y ≈ 1 (ISF y is up).
    let tl = img.px(0, 0);
    assert!(tl[0] < 10 && tl[1] > 240 && tl[2] > 240, "{tl:?}");
    // Bottom-right texel: x ≈ 1, y ≈ 0.
    let br = img.px(W - 1, H - 1);
    assert!(br[0] > 245 && br[1] < 15 && br[2] < 15, "{br:?}");
}

const PARAMS: &str = r#"/*{
    "INPUTS": [
        {"NAME": "level", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5},
        {"NAME": "tint", "TYPE": "color", "DEFAULT": [1.0, 0.0, 0.0, 1.0]},
        {"NAME": "invert", "TYPE": "bool", "DEFAULT": false},
        {"NAME": "channel", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["Red", "Green", "Blue"], "DEFAULT": 0},
        {"NAME": "spot", "TYPE": "point2D", "DEFAULT": [0.0, 0.0]}
    ]
}*/
void main() {
    vec4 c = tint * level;
    if (channel == 1) c = vec4(0.0, level, 0.0, 1.0);
    if (channel == 2) c = vec4(0.0, 0.0, level, 1.0);
    if (invert) c.rgb = 1.0 - c.rgb;
    c.a = spot.x > 0.0 ? spot.x : 1.0;
    gl_FragColor = c;
}
"#;

#[test]
fn inputs_become_parameters() {
    let d = def("Params", PARAMS, None);
    let names: Vec<_> = d.schema.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["level", "tint", "invert", "channel", "spot"]);
    assert!(!d.time_dependent);
    let mut e = engine_or_skip!(vec![d]);
    let n = add(&mut e, "img.isf_params", &[]);
    assert_px(&cook_at(&mut e, n, 0.0), 3, 3, [128, 0, 0, 255], 2);
    set(&mut e, n, "channel", json!("Blue"));
    set(&mut e, n, "level", json!(1.0));
    assert_px(&cook_at(&mut e, n, 0.0), 3, 3, [0, 0, 255, 255], 2);
    set(&mut e, n, "invert", json!(true));
    set(&mut e, n, "spot", json!([0.5, 0.0]));
    assert_px(&cook_at(&mut e, n, 0.0), 3, 3, [255, 255, 0, 128], 2);
}

const FILTER: &str = r#"/*{
    "CATEGORIES": ["Test"],
    "INPUTS": [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "mode", "TYPE": "long", "MIN": 0, "MAX": 2, "DEFAULT": 0}
    ]
}*/
void main() {
    if (mode == 0) gl_FragColor = IMG_THIS_PIXEL(inputImage);
    else if (mode == 1) gl_FragColor = IMG_PIXEL(inputImage, gl_FragCoord.xy);
    else gl_FragColor = vec4(IMG_SIZE(inputImage) / 64.0, 0.0, 1.0);
}
"#;

#[test]
fn filters_sample_their_input_without_flipping_it() {
    let d = def("Pass Through", FILTER, None);
    assert_eq!(d.schema.inputs.len(), 1);
    let mut e = engine_or_skip!(vec![d]);
    let ramp = add(
        &mut e,
        "img.ramp",
        &[
            ("type", json!("vertical")),
            ("colour_b", json!([1.0, 0.0, 0.0, 1.0])),
        ],
    );
    let f = add(&mut e, "img.isf_pass_through", &[]);
    wire(&mut e, ramp, f, "inputImage");
    let source = cook_at(&mut e, ramp, 0.0);
    for mode in 0..2 {
        set(&mut e, f, "mode", json!(mode));
        let out = cook_at(&mut e, f, 0.0);
        for (x, y) in [(0, 0), (5, 2), (W - 1, H - 1), (17, 9)] {
            assert_px(&out, x, y, source.px(x, y), 2);
        }
    }
    set(&mut e, f, "mode", json!(2));
    assert_px(&cook_at(&mut e, f, 0.0), 0, 0, [128, 64, 0, 255], 2);
}

const FEEDBACK: &str = r#"/*{
    "INPUTS": [{"NAME": "step", "TYPE": "float", "DEFAULT": 0.125}],
    "PASSES": [
        {"TARGET": "acc", "PERSISTENT": true, "FLOAT": true},
        {"TARGET": "small", "WIDTH": "$WIDTH/4.0", "HEIGHT": "floor($HEIGHT/4.0)"},
        {}
    ]
}*/
void main() {
    if (PASSINDEX == 0) {
        gl_FragColor = IMG_THIS_PIXEL(acc) + vec4(step, 0.0, 0.0, 0.0);
    } else if (PASSINDEX == 1) {
        gl_FragColor = vec4(RENDERSIZE / 255.0, 0.0, 1.0);
    } else {
        vec4 a = IMG_THIS_PIXEL(acc);
        vec4 s = IMG_NORM_PIXEL(small, vec2(0.5));
        gl_FragColor = vec4(a.r, s.r, s.g, 1.0);
    }
}
"#;

#[test]
fn persistent_targets_accumulate_and_sized_targets_resize() {
    let d = def("Feedback", FEEDBACK, None);
    assert!(
        d.time_dependent,
        "persistent buffers make it time-dependent"
    );
    let mut e = engine_or_skip!(vec![d]);
    let n = add(&mut e, "img.isf_feedback", &[]);
    let mut last = 0;
    for frame in 1..=4 {
        let img = cook_at(&mut e, n, f64::from(frame) / 60.0);
        let r = img.px(4, 4)[0];
        let want = (f64::from(frame) * 0.125 * 255.0).round() as u8;
        assert!(r.abs_diff(want) <= 2, "frame {frame}: r = {r}, want {want}");
        assert!(r > last);
        last = r;
        // The quarter-size pass rendered at 8×4.
        assert_px(&img, 4, 4, [r, 8, 4, 255], 1);
    }
}

const VARYING_FS: &str = r#"/*{"INPUTS": [{"NAME": "shift", "TYPE": "float", "DEFAULT": 0.25}]}*/
varying vec2 shifted;
void main() { gl_FragColor = vec4(shifted, 0.0, 1.0); }
"#;

const VARYING_VS: &str = r#"
varying vec2 shifted;
void main() {
    isf_vertShaderInit();
    shifted = isf_FragNormCoord + vec2(shift, 0.0);
}
"#;

#[test]
fn vertex_shaders_feed_varyings() {
    let mut e = engine_or_skip!(vec![def("Varying", VARYING_FS, Some(VARYING_VS))]);
    let n = add(&mut e, "img.isf_varying", &[]);
    let img = cook_at(&mut e, n, 0.0);
    let x0 = img.px(0, 0)[0];
    assert!(x0.abs_diff(66) <= 3, "x at the left edge is shifted: {x0}");
    assert!(img.px(0, 0)[1] > 240);
}

const AUDIO: &str = r#"/*{
    "INPUTS": [{"NAME": "waveImage", "TYPE": "audio"}]
}*/
void main() {
    float ch0 = IMG_NORM_PIXEL(waveImage, vec2(0.5, 0.25)).r;
    float ch1 = IMG_NORM_PIXEL(waveImage, vec2(0.5, 0.75)).r;
    gl_FragColor = vec4(ch0, ch1, 0.0, 1.0);
}
"#;

#[test]
fn audio_inputs_read_channel_images_with_channel_0_at_the_bottom() {
    let d = def("Wave", AUDIO, None);
    assert_eq!(d.schema.inputs[0].name, "waveImage");
    let mut e = engine_or_skip!(vec![d]);
    let sig = add(
        &mut e,
        "sig.constant",
        &[
            ("name1", json!("left")),
            ("value1", json!(0.2)),
            ("name2", json!("right")),
            ("value2", json!(0.9)),
        ],
    );
    let image = add(&mut e, "img.channel_image", &[]);
    let wave = add(&mut e, "img.isf_wave", &[]);
    wire(&mut e, sig, image, "sig");
    wire(&mut e, image, wave, "waveImage");
    assert_px(&cook_at(&mut e, wave, 0.0), 7, 7, [51, 230, 0, 255], 2);
}
