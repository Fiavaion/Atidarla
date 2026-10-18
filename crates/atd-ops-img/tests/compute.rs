//! The Compute operator (FR-K5) on the GPU: a blur and an iterated diffusion
//! compared with CPU references, persistent state and Reset, compile errors
//! that keep the last good shader, a file that reloads, and every storage
//! format on the real device.

use std::sync::{Mutex, MutexGuard};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

/// One test at a time: two software (WARP) devices cooking compute passes
/// in parallel crash or stall (see `tests/analyze.rs`).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const W: u32 = 24;
const H: u32 = 16;

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
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: W,
        height: H,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name("n");
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

fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

fn pixels(e: &Engine, node: NodeId) -> Vec<u8> {
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()
}

/// Cooks until `until` holds (compilation runs on a worker thread).
fn settle(e: &mut Engine, node: NodeId, until: impl Fn(&Engine, &[u8]) -> bool) -> Vec<u8> {
    let start = std::time::Instant::now();
    loop {
        e.cook(&[node], 0.0).unwrap();
        let px = pixels(e, node);
        if until(e, &px) {
            return px;
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "settled: {:?}, error {:?}",
            &px[..4],
            e.error(node)
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Waits until the node's program is the one for the current code.
fn compiled(e: &mut Engine, node: NodeId) -> Vec<u8> {
    settle(e, node, |e, _| e.error(node).is_none() && !e.pending())
}

/// A colourful, static source image.
fn source(e: &mut Engine) -> NodeId {
    add(
        e,
        "img.noise",
        &[("monochrome", json!(false)), ("scale", json!(6.0))],
    )
}

fn to_f32(px: &[u8]) -> Vec<f32> {
    px.iter().map(|&v| f32::from(v) / 255.0).collect()
}

fn at(img: &[f32], x: i64, y: i64, c: usize) -> f32 {
    let x = x.clamp(0, i64::from(W) - 1) as usize;
    let y = y.clamp(0, i64::from(H) - 1) as usize;
    img[(y * W as usize + x) * 4 + c]
}

fn assert_close(got: &[u8], want: &[f32], what: &str) {
    assert_eq!(got.len(), want.len());
    let mut worst = 0;
    for (g, w) in got.iter().zip(want) {
        let w = (w.clamp(0.0, 1.0) * 255.0).round() as i32;
        worst = worst.max((i32::from(*g) - w).abs());
    }
    assert!(worst <= 1, "{what}: off by up to {worst}/255");
}

const BLUR: &str = "#bind input in0
#bind output dst rgba16float

// 3x3 box blur, edges clamped.
fn kernel(id: vec2u) {
    let last = vec2i(size()) - 1;
    var sum = vec4f(0.0);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let p = clamp(vec2i(id) + vec2i(dx, dy), vec2i(0), last);
            sum += textureLoad(in0, p, 0);
        }
    }
    textureStore(dst, id, sum / 9.0);
}
";

#[test]
fn a_blur_matches_the_cpu() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let src = source(&mut e);
    let c = add(&mut e, "img.compute", &[("code", json!(BLUR))]);
    wire(&mut e, src, c, "in0");
    let got = compiled(&mut e, c);
    let input = to_f32(&pixels(&e, src));
    let mut want = vec![0.0; input.len()];
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            for ch in 0..4 {
                let mut s = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        s += at(&input, x + dx, y + dy, ch);
                    }
                }
                want[(y as usize * W as usize + x as usize) * 4 + ch] = s / 9.0;
            }
        }
    }
    assert_close(&got, &want, "blur");
    // Every tile size gives the same image.
    for tile in atd_ops_img::compute::TILES {
        set(&mut e, c, "tile", json!(tile));
        let px = compiled(&mut e, c);
        assert_eq!(px, got, "tile {tile}");
    }
}

const DIFFUSE: &str = "#bind state heat rgba32float from=in0
#bind param p1 rate f32

// One explicit diffusion step per iteration (5-point Laplacian).
fn kernel(id: vec2u) {
    let p = vec2i(id);
    let last = vec2i(size()) - 1;
    let c = textureLoad(heat, p, 0);
    let l = textureLoad(heat, clamp(p - vec2i(1, 0), vec2i(0), last), 0);
    let r = textureLoad(heat, clamp(p + vec2i(1, 0), vec2i(0), last), 0);
    let u = textureLoad(heat, clamp(p - vec2i(0, 1), vec2i(0), last), 0);
    let d = textureLoad(heat, clamp(p + vec2i(0, 1), vec2i(0), last), 0);
    textureStore(heat_out, id, c + rate() * (l + r + u + d - 4.0 * c));
}
";

#[test]
fn iterated_diffusion_matches_the_cpu() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let src = source(&mut e);
    let c = add(
        &mut e,
        "img.compute",
        &[
            ("code", json!(DIFFUSE)),
            ("iterations", json!(8)),
            ("p1", json!([0.2, 0.0, 0.0, 0.0])),
        ],
    );
    wire(&mut e, src, c, "in0");
    let got = compiled(&mut e, c);
    assert_eq!(e.status(c), Some("P1 = rate: f32"));
    let mut img = to_f32(&pixels(&e, src));
    for _ in 0..8 {
        let mut next = img.clone();
        for y in 0..i64::from(H) {
            for x in 0..i64::from(W) {
                for ch in 0..4 {
                    let v = at(&img, x, y, ch);
                    let lap = at(&img, x - 1, y, ch)
                        + at(&img, x + 1, y, ch)
                        + at(&img, x, y - 1, ch)
                        + at(&img, x, y + 1, ch)
                        - 4.0 * v;
                    next[(y as usize * W as usize + x as usize) * 4 + ch] = v + 0.2 * lap;
                }
            }
        }
        img = next;
    }
    assert_close(&got, &img, "8 diffusion steps");
    // `from=` reloads the input each cook: cooking again gives the same.
    e.cook(&[c], 1.0).unwrap();
    assert_eq!(pixels(&e, c), got);
    // One iteration differs.
    set(&mut e, c, "iterations", json!(1));
    let one = compiled(&mut e, c);
    assert_ne!(one, got);
}

const COUNT: &str = "#bind state acc r32float
#bind buffer hits pixels u32

// Adds 2/255 per iteration and counts dispatches per pixel; persists.
fn kernel(id: vec2u) {
    let i = id.y * size().x + id.x;
    hits[i] += 1u;
    let v = textureLoad(acc, vec2i(id), 0).r + 2.0 / 255.0;
    // The buffer and the image must agree.
    let ok = abs(f32(hits[i]) * 2.0 / 255.0 - v) < 1e-4;
    textureStore(acc_out, id, vec4f(select(1.0, v, ok), 0.0, 0.0, 1.0));
}
";

#[test]
fn state_and_buffers_persist_until_reset() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let c = add(
        &mut e,
        "img.compute",
        &[("code", json!(COUNT)), ("iterations", json!(5))],
    );
    let red = |px: &[u8]| i32::from(px[0]);
    let first = red(&settle(&mut e, c, |e, px| {
        e.error(c).is_none() && px[0] > 0
    }));
    e.cook(&[c], 0.0).unwrap();
    let second = red(&pixels(&e, c));
    assert_eq!(second - first, 10, "5 iterations of 2/255 per cook");
    // Every pixel agrees (r32float shows as red; g, b 0, a 1).
    let px = pixels(&e, c);
    for p in px.chunks_exact(4) {
        assert_eq!(p, [second as u8, 0, 0, 255]);
    }
    // Reset clears the image and the buffer.
    set(&mut e, c, "reset", json!(true));
    e.cook(&[c], 0.0).unwrap();
    assert_eq!(red(&pixels(&e, c)), 10);
    // Editing the code keeps the state when the bindings are the same.
    set(&mut e, c, "code", json!(format!("{COUNT}\n// edited\n")));
    compiled(&mut e, c);
    let after = red(&pixels(&e, c));
    assert!(after > 10, "state kept across the edit: {after}");
}

#[test]
fn errors_name_line_and_column_and_keep_the_last_shader() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let src = source(&mut e);
    let c = add(&mut e, "img.compute", &[("code", json!(BLUR))]);
    wire(&mut e, src, c, "in0");
    let good = compiled(&mut e, c);

    // A WGSL syntax error, a directive error and a validation error.
    let broken = BLUR.replace("sum / 9.0);", "sum / 9.0;");
    set(&mut e, c, "code", json!(broken));
    let px = settle(&mut e, c, |e, _| e.error(c).is_some());
    let err = e.error(c).unwrap().to_owned();
    assert!(err.starts_with("line 14, col "), "{err}");
    assert_eq!(px, good, "the last good shader keeps running");
    set(
        &mut e,
        c,
        "code",
        json!(BLUR.replace("#bind output dst rgba16float", "#bind output dst rgb8")),
    );
    settle(&mut e, c, |e, _| {
        e.error(c)
            .is_some_and(|m| m.starts_with("line 2, col 18: `rgb8`"))
    });
    set(&mut e, c, "code", json!(BLUR.replace("sum / 9.0", "1.0")));
    settle(&mut e, c, |e, _| {
        e.error(c).is_some_and(|m| m.starts_with("line 14, col "))
    });
    // A file reloads when it changes.
    let path = std::env::temp_dir().join(format!("atd-compute-{}.wgsl", std::process::id()));
    let fill = |rgba: &str| {
        format!(
            "#bind output dst\nfn kernel(id: vec2u) {{ textureStore(dst, id, vec4f({rgba})); }}\n"
        )
    };
    std::fs::write(&path, fill("0.0, 0.0, 1.0, 1.0")).unwrap();
    set(&mut e, c, "file", json!(path.to_string_lossy()));
    settle(&mut e, c, |e, px| {
        px[..4] == [0, 0, 255, 255] && e.error(c).is_none()
    });
    std::fs::write(&path, fill("1.0, 1.0, 0.0, 1.0")).unwrap();
    settle(&mut e, c, |_, px| px[..4] == [255, 255, 0, 255]);
    std::fs::remove_file(&path).ok();
    settle(&mut e, c, |e, _| {
        e.error(c).is_some_and(|m| m.contains("atd-compute"))
    });
}

/// Every storage format the directives allow compiles and runs on this
/// device, four writable images at once (the guaranteed maximum).
#[test]
fn every_storage_format_runs_on_this_device() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let code = "#bind state a rgba8unorm
#bind state b rgba16float
#bind state c rgba32float
#bind state d r32float
fn kernel(id: vec2u) {
    let v = vec4f(0.6, 0.2, 1.0, 1.0);
    textureStore(a_out, id, v);
    textureStore(b_out, id, v + textureLoad(b, vec2i(id), 0) * 0.0);
    textureStore(c_out, id, v);
    textureStore(d_out, id, v);
}
";
    let c = add(&mut e, "img.compute", &[("code", json!(code))]);
    let px = compiled(&mut e, c);
    // The node shows the first state, rgba8unorm.
    assert_eq!(px[..4], [153, 51, 255, 255]);
}
