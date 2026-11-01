//! Neural CA on the GPU: the same steps as a CPU reference, growth and
//! regrowth after erasing, file weights equal to the built-in rule, and the
//! example's generated weights file.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_interop::readback::read_rgba8;
use lsr_ops_img::nca::{self, GENERATED_LICENCE, GENERATED_SOURCE, Weights};
use serde_json::{Value, json};

/// One test at a time: two software (WARP) devices cooking compute passes
/// in parallel crash or stall (see `tests/analyze.rs`).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn engine(w: u32, h: u32) -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: w,
        height: h,
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

fn pixels(e: &Engine, node: NodeId) -> Vec<u8> {
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()
}

/// Cooks until the node shows something (its pipelines build on a worker
/// thread); returns the first image it showed.
fn first_frame(e: &mut Engine, nodes: &[NodeId]) -> Vec<Vec<u8>> {
    let start = std::time::Instant::now();
    loop {
        e.cook(nodes, 0.0).unwrap();
        let imgs: Vec<Vec<u8>> = nodes.iter().map(|n| pixels(e, *n)).collect();
        let shown = imgs.iter().all(|px| px.chunks_exact(4).any(|p| p[3] > 0));
        if shown {
            for n in nodes {
                assert_eq!(e.error(*n), None);
            }
            return imgs;
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "Neural CA showed nothing (errors: {:?})",
            nodes.iter().map(|n| e.error(*n)).collect::<Vec<_>>()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// ------------------------------------------------------- CPU reference

fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277803737);
    (w >> 22) ^ w
}

fn rand(seed: u32, x: u32, y: u32, z: u32) -> f32 {
    (pcg(x ^ pcg(y ^ pcg(z ^ pcg(seed)))) >> 8) as f32 / 16777216.0
}

struct Cpu {
    w: Weights,
    gw: usize,
    gh: usize,
    seed: u32,
    fire: f32,
    threshold: f32,
    state: Vec<f32>,
    step: u32,
}

impl Cpu {
    fn at(&self, s: &[f32], x: i64, y: i64, c: usize) -> f32 {
        if x < 0 || y < 0 || x >= self.gw as i64 || y >= self.gh as i64 {
            return 0.0;
        }
        s[(y as usize * self.gw + x as usize) * self.w.channels + c]
    }

    fn alive(&self, s: &[f32], x: i64, y: i64) -> bool {
        let mut m = 0.0f32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                m = m.max(self.at(s, x + dx, y + dy, 3));
            }
        }
        m > self.threshold
    }

    /// The `noise` seed: every channel uniform in [0, 1).
    fn seed_noise(&mut self) {
        let c = self.w.channels;
        for y in 0..self.gh {
            for x in 0..self.gw {
                for ch in 0..c {
                    self.state[(y * self.gw + x) * c + ch] =
                        rand(self.seed, x as u32, y as u32, 0xfffffff1 + ch as u32);
                }
            }
        }
    }

    fn step(&mut self) {
        let (c, h) = (self.w.channels, self.w.hidden);
        let p = 4 * c;
        let s = self.state.clone();
        let mut next = vec![0.0; s.len()];
        for y in 0..self.gh as i64 {
            for x in 0..self.gw as i64 {
                let mut per = vec![0.0f32; p];
                for ch in 0..c {
                    let v: Vec<f32> = (0..9)
                        .map(|k| self.at(&s, x + k % 3 - 1, y + k / 3 - 1, ch))
                        .collect();
                    per[ch * 4] = v[4];
                    per[ch * 4 + 1] =
                        ((v[2] + 2.0 * v[5] + v[8]) - (v[0] + 2.0 * v[3] + v[6])) / 8.0;
                    per[ch * 4 + 2] =
                        ((v[6] + 2.0 * v[7] + v[8]) - (v[0] + 2.0 * v[1] + v[2])) / 8.0;
                    per[ch * 4 + 3] = ((v[0] + v[2] + v[6] + v[8])
                        + 2.0 * (v[1] + v[3] + v[5] + v[7])
                        - 12.0 * v[4])
                        / 16.0;
                }
                let mut ds = self.w.b2.clone();
                for j in 0..h {
                    let mut a = self.w.b1[j];
                    for (i, pv) in per.iter().enumerate() {
                        a += self.w.w1[j * p + i] * pv;
                    }
                    let hv = a.max(0.0);
                    for (ch, d) in ds.iter_mut().enumerate() {
                        *d += self.w.w2[ch * h + j] * hv;
                    }
                }
                let fire = rand(self.seed, x as u32, y as u32, self.step) < self.fire;
                let live = self.alive(&s, x, y);
                for ch in 0..c {
                    let mut v = per[ch * 4];
                    if fire {
                        v += ds[ch];
                    }
                    next[(y as usize * self.gw + x as usize) * c + ch] = if live { v } else { 0.0 };
                }
            }
        }
        let mut masked = next.clone();
        for y in 0..self.gh as i64 {
            for x in 0..self.gw as i64 {
                if !self.alive(&next, x, y) {
                    let i = (y as usize * self.gw + x as usize) * c;
                    masked[i..i + c].fill(0.0);
                }
            }
        }
        self.state = masked;
        self.step += 1;
    }

    /// The `rgba` display over opaque black, as 8-bit floats 0..1.
    fn display(&self) -> Vec<f32> {
        let c = self.w.channels;
        let mut out = Vec::new();
        for i in 0..self.gw * self.gh {
            for ch in 0..3 {
                out.push(self.state[i * c + ch].clamp(0.0, 1.0));
            }
            out.push(1.0);
        }
        out
    }
}

#[test]
fn steps_match_a_cpu_reference() {
    let _serial = serial();
    let (gw, gh) = (32, 24);
    let Some(mut e) = engine(gw as u32, gh as u32) else {
        return;
    };
    let n = add(
        &mut e,
        "img.nca",
        &[
            ("cell_size", json!(1)),
            ("seed_mode", json!("noise")),
            ("steps", json!(4)),
            ("random_seed", json!(7)),
        ],
    );
    let mut cpu = Cpu {
        w: nca::GROW.weights(),
        gw,
        gh,
        seed: 7,
        fire: 0.5,
        threshold: 0.1,
        state: vec![0.0; gw * gh * 8],
        step: 0,
    };
    cpu.seed_noise();
    let mut got = first_frame(&mut e, &[n]).remove(0);
    for frame in 0..4 {
        for _ in 0..4 {
            cpu.step();
        }
        let want = cpu.display();
        let mut worst = 0;
        let mut off = 0;
        for (g, w) in got.iter().zip(&want) {
            let d = (i32::from(*g) - (w * 255.0).round() as i32).abs();
            worst = worst.max(d);
            if d > 1 {
                off += 1;
            }
        }
        assert_eq!(
            off, 0,
            "frame {frame}: {off} values off by more than 1/255 (worst {worst})"
        );
        e.cook(&[n], 0.0).unwrap();
        got = pixels(&e, n);
    }
}

fn coverage(px: &[u8]) -> usize {
    px.chunks_exact(4)
        .filter(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) > 30)
        .count()
}

#[test]
fn grows_from_a_seed_and_regrows_where_erased() {
    let _serial = serial();
    let Some(mut e) = engine(64, 64) else { return };
    let n = add(
        &mut e,
        "img.nca",
        &[("cell_size", json!(1)), ("steps", json!(64))],
    );
    first_frame(&mut e, &[n]);
    let mut grown = Vec::new();
    for _ in 0..12 {
        e.cook(&[n], 0.0).unwrap();
        grown.push(coverage(&pixels(&e, n)));
    }
    assert!(
        grown.windows(2).all(|w| w[1] >= w[0]) && grown[11] > grown[0] + 100,
        "grows: {grown:?}"
    );
    // Erase the middle for one frame.
    let hole = add(
        &mut e,
        "img.shape",
        &[("size", json!([0.3, 0.3])), ("softness", json!(0.0))],
    );
    e.edit(
        "erase",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(hole, "out"), Endpoint::new(n, "erase")),
        }],
    )
    .unwrap();
    // Lit pixels within 6 of the centre.
    let centre = |px: &[u8]| {
        let mut lit = 0;
        for y in 26..38 {
            for x in 26..38 {
                let i = (y * 64 + x) * 4;
                if (x as i32 - 32).pow(2) + (y as i32 - 32).pow(2) <= 36
                    && u32::from(px[i]) + u32::from(px[i + 1]) + u32::from(px[i + 2]) > 30
                {
                    lit += 1;
                }
            }
        }
        lit
    };
    assert!(centre(&pixels(&e, n)) > 50, "the centre is grown");
    e.cook(&[n], 0.0).unwrap();
    assert_eq!(centre(&pixels(&e, n)), 0, "the centre is erased");
    e.edit(
        "unerase",
        [Edit::Disconnect {
            wire: Wire::new(Endpoint::new(hole, "out"), Endpoint::new(n, "erase")),
        }],
    )
    .unwrap();
    for _ in 0..12 {
        e.cook(&[n], 0.0).unwrap();
    }
    let regrown = centre(&pixels(&e, n));
    assert!(regrown > 50, "the centre grew back: {regrown} lit");
}

#[test]
fn file_weights_run_like_the_built_in_rule() {
    let _serial = serial();
    let Some(mut e) = engine(48, 32) else { return };
    let path = std::env::temp_dir().join(format!("lsr-nca-{}.json", std::process::id()));
    // CRLF, as a Windows checkout may have it.
    let text = nca::GROW
        .weights()
        .to_json("test", "test")
        .replace('\n', "\r\n");
    std::fs::write(&path, text).unwrap();
    let common = [
        ("steps", json!(16)),
        ("seed_mode", json!("scatter")),
        ("density", json!(0.01)),
    ];
    let built_in = add(&mut e, "img.nca", &common);
    let mut with_file = common.to_vec();
    with_file.extend([
        ("rule", json!("file")),
        ("weights", json!(path.to_string_lossy())),
    ]);
    let file = add(&mut e, "img.nca", &with_file);
    // The file loads a frame or more later: restart both together.
    first_frame(&mut e, &[built_in, file]);
    set(&mut e, built_in, "reset", json!(true));
    set(&mut e, file, "reset", json!(true));
    for frame in 0..6 {
        e.cook(&[built_in, file], 0.0).unwrap();
        assert!(
            pixels(&e, built_in) == pixels(&e, file),
            "frame {frame}: same weights and seed give the same image"
        );
    }
    // A broken file is reported, and the node keeps its last image (as
    // every operator with an error does); a fixed file carries on from the
    // state the last good weights left.
    std::fs::write(&path, "{ \"format\": \"lasair-nca\" ").unwrap();
    let start = std::time::Instant::now();
    while e.error(file).is_none() {
        e.cook(&[built_in, file], 0.0).unwrap();
        assert!(start.elapsed().as_secs() < 30, "the bad file was noticed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        e.error(file).unwrap().starts_with("weights file: "),
        "{:?}",
        e.error(file)
    );
    let before = pixels(&e, file);
    e.cook(&[built_in, file], 0.0).unwrap();
    assert!(pixels(&e, file) == before, "the last image stays");
    std::fs::write(&path, nca::GROW.weights().to_json("test", "test")).unwrap();
    let start = std::time::Instant::now();
    while e.error(file).is_some() {
        e.cook(&[built_in, file], 0.0).unwrap();
        assert!(start.elapsed().as_secs() < 30, "the fixed file was noticed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(pixels(&e, file) != before, "running again");
    std::fs::remove_file(&path).ok();
    // The `spots` rule differs; `hidden` shows other channels.
    set(&mut e, file, "rule", json!("spots"));
    for _ in 0..4 {
        e.cook(&[built_in, file], 0.0).unwrap();
    }
    assert_eq!(e.error(file), None);
    assert!(pixels(&e, built_in) != pixels(&e, file), "spots differ");
    set(&mut e, file, "display", json!("hidden"));
    e.cook(&[file], 0.0).unwrap();
    let hidden = pixels(&e, file);
    assert!(hidden.chunks_exact(4).all(|p| p[3] == 255));
}

/// Frame time (CPU recording plus GPU completion) at 1280×720, cell size 2
/// (a 640×360 grid), 8 steps: the cost behind the cook budget. Run with
/// `cargo test --release -p lsr-ops-img --test nca -- --ignored --nocapture frame_time`.
#[test]
#[ignore = "a measurement; prints the frame time"]
fn frame_time() {
    let _serial = serial();
    let Some(mut e) = engine(1280, 720) else {
        return;
    };
    let n = add(&mut e, "img.nca", &[("seed_mode", json!("noise"))]);
    first_frame(&mut e, &[n]);
    for _ in 0..10 {
        e.cook(&[n], 0.0).unwrap();
    }
    let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
    let frames = 60;
    let start = std::time::Instant::now();
    for _ in 0..frames {
        e.cook(&[n], 0.0).unwrap();
        let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);
    let cpu = e.stat(n).map(|s| s.last_us);
    eprintln!("Neural CA 640×360 grid, 8 steps: {ms:.2} ms per frame (CPU cook {cpu:?} µs)");
}

const EXAMPLE_WEIGHTS: &str = "../../examples/neural-growth.lasair/nca/coral.json";

fn example_weights() -> String {
    nca::CORAL
        .weights()
        .to_json(GENERATED_LICENCE, GENERATED_SOURCE)
}

/// Rule 4: the example ships only weights this generator writes.
#[test]
fn the_example_weights_are_generated() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(EXAMPLE_WEIGHTS);
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(
        on_disk,
        example_weights(),
        "regenerate with `cargo test -p lsr-ops-img --test nca -- --ignored write_example_weights`"
    );
}

/// Writes the example's weights file (the deterministic generator).
#[test]
#[ignore = "writes into examples/; run by hand after changing GrowRule"]
fn write_example_weights() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(EXAMPLE_WEIGHTS);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, example_weights()).unwrap();
}
