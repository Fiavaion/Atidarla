//! Live AI through the whole engine path (PT-3, PT-4, PT-5 hand checks):
//! animated noise → Diffusion → governed output, cooked at 60 fps on the
//! local AI Host (or `--route`). Prints the node's status each second, then
//! the effective AI fps, latency and the slowest cook.
//!
//! `cargo run -p lsr-ops-ai --release --example ai_bench -- [--model sd-turbo]
//!  [--seconds 20] [--size 512] [--route local] [--steps 1]`

use std::time::{Duration, Instant};

use lsr_ai::supervisor::Supervisor;
use lsr_core::graph::{Edit, Endpoint, Node, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use serde_json::json;

fn main() {
    let mut model = "sd-turbo".to_owned();
    let mut seconds = 20.0;
    let mut size = 512;
    let mut route = "local".to_owned();
    let mut steps = 1;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next().expect("a value");
        match a.as_str() {
            "--model" => model = v,
            "--seconds" => seconds = v.parse().expect("seconds"),
            "--size" => size = v.parse().expect("size"),
            "--route" => route = v,
            "--steps" => steps = v.parse().expect("steps"),
            _ => panic!("unknown argument {a}"),
        }
    }
    let instance = lsr_gpu::primary_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("a GPU");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_ai::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_ai::library(&mut lib);
    let mut e = Engine::new(
        device,
        queue,
        reg,
        lib,
        EngineConfig {
            width: 1280,
            height: 720,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    let (src, ai) = (e.alloc_id(), e.alloc_id());
    let set = |node, p: &str, v| Edit::SetParam {
        node,
        param: p.into(),
        value: Some(Param::constant(v)),
    };
    e.edit(
        "patch",
        [
            Edit::AddNode {
                node: Node::new(src, "img.noise", String::from("noise")),
            },
            Edit::AddNode {
                node: Node::new(ai, "ai.diffusion", String::from("diffusion")),
            },
            set(ai, "model", json!(model)),
            set(ai, "route", json!(route)),
            set(ai, "width", json!(size)),
            set(ai, "height", json!(size)),
            set(ai, "steps", json!(steps)),
            set(
                ai,
                "prompt1",
                json!("a watercolour painting of a stormy sea"),
            ),
            Edit::Connect {
                wire: Wire::new(Endpoint::new(src, "out"), Endpoint::new(ai, "image")),
            },
        ],
    )
    .expect("patch");
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let t0 = Instant::now();
    let mut last_print = t0;
    let mut worst = Duration::ZERO;
    let mut cooks = 0u64;
    let mut late = 0u64;
    while t0.elapsed().as_secs_f64() < seconds {
        let c = Instant::now();
        e.cook_frame(Some(ai), t0.elapsed().as_secs_f64())
            .expect("cook");
        let took = c.elapsed();
        // Count only once the model runs (loading is not show time).
        if e.status(ai).is_some_and(|s| s.contains("AI fps")) {
            worst = worst.max(took);
            cooks += 1;
            late += u64::from(took > frame);
        }
        if last_print.elapsed() >= Duration::from_secs(1) {
            last_print = Instant::now();
            println!(
                "{:5.1} s  {}",
                t0.elapsed().as_secs_f64(),
                e.status(ai).unwrap_or("…")
            );
            if let Some(err) = e.error(ai) {
                println!("        error: {err}");
            }
        }
        std::thread::sleep(frame.saturating_sub(c.elapsed()));
    }
    println!(
        "model {model}, {size}², {steps} step(s), route {route}: final status \"{}\"; {cooks} cooks at 60 fps, {late} over 16.7 ms, slowest {worst:?}",
        e.status(ai).unwrap_or("")
    );
    Supervisor::global().shutdown();
}
