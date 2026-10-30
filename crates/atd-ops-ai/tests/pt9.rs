//! PT-9 and M4 AC3 in the engine: an AI node cooks at 60 fps while its host
//! is killed mid-show. No cook waits for the host (every frame is cooked on
//! time), the supervisor restarts it, and results flow again within 5 s.
//!
//! The host is this test binary run again with `ATD_PT9_HOST=1`: the Rust
//! reference host, as a real process that can be killed.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use atd_ai::client::HostCommand;
use atd_ai::refhost::{RefHostOptions, serve, watch_parent};
use atd_ai::supervisor::{HostKind, HostSpec, Supervisor};
use atd_core::graph::{Edit, Endpoint, Node, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::json;

fn main() {
    if std::env::var_os("ATD_PT9_HOST").is_some() {
        host();
        return;
    }
    let sup = supervisor();
    let Some(mut e) = engine(sup) else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("pt9: skipped, no GPU adapter");
        return;
    };
    run(&mut e, sup);
    println!("test pt9 ... ok");
}

/// The reference host, as `atd-ai-refhost` would run it.
fn host() {
    let mut args = std::env::args().skip(1);
    let mut opts = RefHostOptions::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--token" => opts.token = args.next(),
            "--parent-pid" => watch_parent(args.next().unwrap().parse().unwrap()),
            _ => {
                args.next();
            }
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    println!(
        "atd-ai-host listening on {}",
        listener.local_addr().unwrap()
    );
    serve(listener, opts);
}

fn supervisor() -> &'static Supervisor {
    let sup: &'static Supervisor = Box::leak(Box::new(Supervisor::new()));
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Local(HostCommand {
            program: std::env::current_exe().unwrap().into(),
            args: vec![],
            env: vec![("ATD_PT9_HOST".into(), "1".into())],
        }),
    });
    sup
}

fn engine(sup: &'static Supervisor) -> Option<Engine> {
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
    atd_ops_ai::library_with(&mut lib, sup);
    let config = EngineConfig {
        width: 64,
        height: 36,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn run(e: &mut Engine, sup: &'static Supervisor) {
    let src = e.alloc_id();
    let ai = e.alloc_id();
    let set = |node, p: &str, v| Edit::SetParam {
        node,
        param: p.into(),
        value: Some(Param::constant(v)),
    };
    e.edit(
        "patch",
        [
            Edit::AddNode {
                node: Node::new(src, "img.constant", String::from("src")),
            },
            set(src, "colour", json!([0.2, 0.4, 0.6, 1.0])),
            Edit::AddNode {
                node: Node::new(ai, "ai.model", String::from("ai")),
            },
            set(ai, "model", json!("builtin.invert")),
            set(ai, "policy", json!("hold")),
            set(ai, "width", json!(64)),
            set(ai, "height", json!(36)),
            Edit::Connect {
                wire: Wire::new(Endpoint::new(src, "out"), Endpoint::new(ai, "image")),
            },
        ],
    )
    .unwrap();
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let red = |e: &Engine| read_rgba8(e.device(), e.queue(), e.result(ai).unwrap()).unwrap()[0];
    let mut n = 0u64;
    let mut cook = |e: &mut Engine, worst: &mut Duration| {
        let c = Instant::now();
        e.cook_frame(Some(ai), n as f64 / 60.0).unwrap();
        *worst = (*worst).max(c.elapsed());
        n += 1;
        std::thread::sleep(frame.saturating_sub(c.elapsed()));
    };
    // First result: the inverse of 0.2 red is about 204.
    let t0 = Instant::now();
    let mut warm = Duration::ZERO;
    cook(e, &mut warm);
    while red(e) < 150 {
        cook(e, &mut warm);
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "no first result: {:?}",
            e.status(ai)
        );
    }
    let mut worst = Duration::ZERO;
    for _ in 0..60 {
        cook(e, &mut worst);
    }
    // Kill the host and change the input: until results flow again the
    // output holds the last good frame (inverse of the old colour); then
    // the inverse of the new colour (0.9 red → about 25) arrives.
    let host = sup.host("local").unwrap();
    kill(host.pid.expect("local host pid"));
    let killed = Instant::now();
    e.edit("colour", [set(src, "colour", json!([0.9, 0.4, 0.6, 1.0]))])
        .unwrap();
    let back = loop {
        cook(e, &mut worst);
        let r = red(e);
        if r < 60 {
            break killed.elapsed();
        }
        assert!(r > 150, "the output dropped its last good frame (red {r})");
        assert!(
            killed.elapsed() < Duration::from_secs(8),
            "no results after the kill: {:?}",
            e.status(ai)
        );
    };
    let restarted = sup.host("local").unwrap();
    assert_eq!(restarted.restarts, host.restarts + 1);
    println!(
        "PT-9 (engine): results again {back:?} after the host was killed; slowest cook {worst:?}"
    );
    assert!(back < Duration::from_secs(5), "{back:?}");
    // No cook waited for the host: each fits inside a 60 fps frame on real
    // hardware (`ATD_TIMING_STRICT=1`); a shared CI runner gets slack, still
    // far below the seconds a wait for the host would take.
    let strict = std::env::var("ATD_TIMING_STRICT").is_ok_and(|v| v == "1");
    let limit = Duration::from_millis(if strict { 16 } else { 50 });
    assert!(worst < limit, "a cook took {worst:?}");
}

fn kill(pid: u32) {
    let ok = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
    } else {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
    };
    assert!(ok.unwrap().success());
}
