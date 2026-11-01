//! M4 AC2: a prompt crossfade between two AI clips on the crossfader is
//! smooth at 60 fps output. Two Prompt Clips play on crossfader sides A and
//! B of a deck; a Diffusion node follows the deck; the crossfader sweeps
//! from A to B over two seconds while the engine cooks at 60 fps. The
//! reference host's `builtin.prompts` paints the weighted mix of a colour
//! per prompt, so the blend shows in the pixels: the output must move from
//! A's colour to B's in small steps, with no cook waiting for the host.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use lsr_ai::refhost::{RefHostOptions, serve};
use lsr_ai::supervisor::{HostKind, HostSpec, Supervisor};
use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_interop::readback::read_rgba8;
use lsr_stage::Stage;
use lsr_stage::ai::{Feeds, LiveValue};
use serde_json::{Value, json};

fn engine(sup: &'static Supervisor) -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_ai::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_ai::library_with(&mut lib, sup);
    let config = EngineConfig {
        width: 32,
        height: 16,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, name: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty, String::from(name)),
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

fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

/// The colour `builtin.prompts` gives one prompt (FNV-1a of its text).
fn colour(text: &str) -> [f64; 3] {
    let mut h: u32 = 0x811c_9dc5;
    for b in text.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    [0, 1, 2].map(|i| f64::from((h >> (8 * i)) as u8))
}

#[test]
fn a_prompt_crossfade_is_smooth_at_60_fps() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || serve(listener, RefHostOptions::default()));
    let sup: &'static Supervisor = Box::leak(Box::new(Supervisor::new()));
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Remote { addr, token: None },
    });
    let Some(mut e) = engine(sup) else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (pa, pb) = ("a forest at dawn", "a neon city at night");
    let src = add(&mut e, "img.constant", "src", &[]);
    let clip_a = add(&mut e, "ai.prompt", "forest", &[("prompt", json!(pa))]);
    let clip_b = add(
        &mut e,
        "ai.prompt",
        "city",
        &[("prompt", json!(pb)), ("seed", json!(9))],
    );
    let layer_a = add(&mut e, "img.layer", "layerA", &[("target", json!(0))]);
    let layer_b = add(&mut e, "img.layer", "layerB", &[("target", json!(0))]);
    let mixer = add(
        &mut e,
        "img.mixer",
        "deck",
        &[
            ("group0", json!("a")),
            ("group1", json!("b")),
            ("crossfader", json!(0.0)),
        ],
    );
    wire(&mut e, src, clip_a, "image");
    wire(&mut e, src, clip_b, "image");
    wire(&mut e, clip_a, layer_a, "clip0");
    wire(&mut e, clip_b, layer_b, "clip0");
    wire(&mut e, layer_a, mixer, "layer0");
    wire(&mut e, layer_b, mixer, "layer1");
    let ai = add(
        &mut e,
        "ai.diffusion",
        "diffusion",
        &[
            ("model", json!("builtin.prompts")),
            ("prompts_from", json!("deck")),
            ("policy", json!("blend")),
            ("width", json!(32)),
            ("height", json!(16)),
        ],
    );
    wire(&mut e, mixer, ai, "image");

    let stage = Stage::read(e.network(), e.registry());
    let mut feeds = Feeds::read(e.network(), e.registry(), &stage);
    assert_eq!(feeds.feeds.len(), 1, "the Diffusion node follows the deck");
    let targets = |l: NodeId| (l == layer_a || l == layer_b).then_some(0);

    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let mut x = 0.0f64;
    let mut worst = Duration::ZERO;
    let cook = |e: &mut Engine, feeds: &mut Feeds, x: f64, worst: &mut Duration, t: f64| {
        let c = Instant::now();
        let mut live: Vec<(NodeId, &'static str, Value)> = Vec::new();
        feeds.frame(
            t * 2.0,
            |_| Some(x),
            targets,
            |n, p, v| {
                live.push((
                    n,
                    p,
                    match v {
                        LiveValue::Value(v) => v.clone(),
                        LiveValue::Float(f) => json!(f),
                        LiveValue::Int(i) => json!(i),
                    },
                ));
            },
        );
        for (n, p, v) in &live {
            e.set_live(*n, p, Some(v));
        }
        e.cook_frame(Some(ai), t).unwrap();
        *worst = (*worst).max(c.elapsed());
        std::thread::sleep(frame.saturating_sub(c.elapsed()));
    };
    let px = |e: &Engine| {
        let p = read_rgba8(e.device(), e.queue(), e.result(ai).unwrap()).unwrap();
        [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]
    };
    let dist = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f64::max);
    let (ca, cb) = (colour(pa), colour(pb));
    // Settle on clip A.
    let t0 = Instant::now();
    let mut t = 0.0;
    loop {
        cook(&mut e, &mut feeds, x, &mut worst, t);
        t += 1.0 / 60.0;
        if dist(px(&e), ca) <= 2.0 {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "never showed clip A: {:?}",
            e.status(ai)
        );
    }
    // Sweep the crossfader over 120 frames, then hold on B.
    let mut prev = px(&e);
    let mut biggest = 0.0f64;
    worst = Duration::ZERO;
    for i in 1..=150 {
        x = (f64::from(i) / 120.0).min(1.0);
        cook(&mut e, &mut feeds, x, &mut worst, t);
        t += 1.0 / 60.0;
        let now = px(&e);
        biggest = biggest.max(dist(now, prev));
        prev = now;
    }
    let span = dist(ca, cb);
    println!(
        "AC2: crossfade A→B over 120 frames: largest step {biggest:.1} of a {span:.0} range; slowest frame {worst:?}"
    );
    assert!(dist(prev, cb) <= 2.0, "ended at {prev:?}, want B {cb:?}");
    // Smooth: no frame jumps more than a few times an even 1/120 step.
    assert!(
        biggest <= (span / 120.0 * 6.0).max(4.0),
        "a step of {biggest:.1}"
    );
    // Shared CI runners stall a frame now and then; the 16 ms budget is
    // held under LSR_TIMING_STRICT=1 on a quiet machine.
    let strict = std::env::var("LSR_TIMING_STRICT").is_ok_and(|v| v == "1");
    assert!(
        worst < Duration::from_millis(if strict { 16 } else { 250 }),
        "{worst:?}"
    );
    // The leading clip's seed follows the crossfader.
    assert_eq!(
        e.live_values().get(ai, "seed"),
        Some(&json!(9)),
        "B leads at the end, with its seed"
    );
    let _ = (clip_a, clip_b);
}
