//! AI operators in the engine, against the reference host running in this
//! process: results come back and are governed, the render thread never
//! waits, offline renders wait one frame behind, Control Prep draws edges.

use std::net::TcpListener;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use atd_ai::refhost::{RefHostOptions, serve};
use atd_ai::supervisor::{HostKind, HostSpec, Supervisor};
use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

const W: u32 = 32;
const H: u32 = 16;

/// A supervisor whose `local` host is a reference host in this process.
fn supervisor() -> &'static Supervisor {
    static SUP: OnceLock<&'static Supervisor> = OnceLock::new();
    SUP.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || serve(listener, RefHostOptions::default()));
        let sup: &'static Supervisor = Box::leak(Box::new(Supervisor::new()));
        sup.set(HostSpec {
            name: "local".into(),
            kind: HostKind::Remote { addr, token: None },
        });
        sup
    })
}

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_ai::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_ai::library_with(&mut lib, supervisor());
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

fn pixel(e: &Engine, node: NodeId) -> [u8; 4] {
    let px = read_rgba8(e.device(), e.queue(), e.result(node).expect("result")).unwrap();
    px[..4].try_into().unwrap()
}

#[track_caller]
fn near(got: [u8; 4], want: [u8; 4]) {
    assert!(
        got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 2),
        "got {got:?}, want {want:?}"
    );
}

fn inverting(e: &mut Engine, policy: &str) -> NodeId {
    let src = add(
        e,
        "img.constant",
        &[("colour", json!([0.2, 0.4, 0.6, 1.0]))],
    );
    let ai = add(
        e,
        "ai.model",
        &[
            ("model", json!("builtin.invert")),
            ("policy", json!(policy)),
            ("width", json!(W)),
            ("height", json!(H)),
            ("effect", json!(0.0)),
        ],
    );
    wire(e, src, ai, "image");
    ai
}

/// Before the first result the input shows through; then the inverse
/// arrives, and no cook waited for the host.
#[test]
fn results_arrive_without_the_cook_waiting() {
    let mut e = engine_or_skip!();
    let ai = inverting(&mut e, "hold");
    let input = [51, 102, 153, 255];
    e.cook(&[ai], 0.0).unwrap();
    assert_eq!(e.error(ai), None);
    near(pixel(&e, ai), input);
    let t0 = Instant::now();
    let mut worst = Duration::ZERO;
    let mut frame = 1;
    loop {
        let c = Instant::now();
        e.cook(&[ai], frame as f64 / 60.0).unwrap();
        worst = worst.max(c.elapsed());
        frame += 1;
        let px = pixel(&e, ai);
        if px[0] > 150 {
            near(px, [204, 153, 102, 255]);
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "no result: {:?}",
            e.status(ai)
        );
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(worst < Duration::from_millis(250), "a cook took {worst:?}");
    // The status line names the host and the rate.
    std::thread::sleep(Duration::from_millis(600));
    e.cook(&[ai], frame as f64 / 60.0).unwrap();
    let status = e.status(ai).unwrap_or_default().to_owned();
    assert!(status.starts_with("local · "), "{status}");
}

/// Offline, frame `i` shows the result for frame `i - 1`, waited for.
#[test]
fn offline_frames_wait_for_their_results() {
    let mut e = engine_or_skip!();
    e.set_mode(CookMode::Offline { fps: 30.0 });
    let ai = inverting(&mut e, "blend");
    e.cook(&[ai], 0.0).unwrap();
    near(pixel(&e, ai), [51, 102, 153, 255]);
    for i in 1..4 {
        e.cook(&[ai], f64::from(i) / 30.0).unwrap();
        assert_eq!(e.error(ai), None);
        near(pixel(&e, ai), [204, 153, 102, 255]);
    }
}

/// A route nobody can serve reports why instead of failing.
#[test]
fn an_unserved_route_says_why() {
    let mut e = engine_or_skip!();
    let ai = inverting(&mut e, "hold");
    e.edit(
        "route",
        [Edit::SetParam {
            node: ai,
            param: "route".into(),
            value: Some(Param::constant(json!("lan:nowhere"))),
        }],
    )
    .unwrap();
    let t0 = Instant::now();
    loop {
        e.cook(&[ai], t0.elapsed().as_secs_f64()).unwrap();
        assert_eq!(e.error(ai), None);
        if let Some(s) = e.status(ai)
            && s.contains("lan:nowhere: not configured")
        {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", e.status(ai));
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn control_prep_finds_edges_and_thresholds() {
    let mut e = engine_or_skip!();
    let flat = add(
        &mut e,
        "img.constant",
        &[("colour", json!([1.0, 1.0, 1.0, 1.0]))],
    );
    let canny = add(&mut e, "ai.control_prep", &[]);
    wire(&mut e, flat, canny, "image");
    e.cook(&[canny], 0.0).unwrap();
    near(pixel(&e, canny), [0, 0, 0, 255]);
    let thr = add(
        &mut e,
        "ai.control_prep",
        &[("mode", json!("threshold")), ("high", json!(0.5))],
    );
    wire(&mut e, flat, thr, "image");
    e.cook(&[thr], 0.0).unwrap();
    near(pixel(&e, thr), [255, 255, 255, 255]);
    // A vertical edge: half black, half white.
    let ramp = add(&mut e, "img.ramp", &[("type", json!("horizontal"))]);
    let step = add(&mut e, "ai.control_prep", &[("mode", json!("soft_edge"))]);
    wire(&mut e, ramp, step, "image");
    e.cook(&[step], 0.0).unwrap();
    assert_eq!(e.error(step), None);
}
