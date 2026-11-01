//! Share Out and Share In over Spout (Windows) and Syphon (macOS; rule 7):
//! pixels survive a loopback in one engine and between two engines, the
//! sender goes when the node does or is disabled, Share In follows a
//! resize, and bad names are node errors. Linux's behaviour is a unit test
//! in `src/share/mod.rs`.
//!
//! Spout orders nothing between two D3D11 devices beyond `Flush`: hardware
//! drivers sync legacy shared textures implicitly, WARP (CI) does not. So
//! these tests wait for Share Out's copy to finish on the GPU before a
//! Share In reads, as `lsr-interop`'s `spout_loopback` tests do; in real
//! use a receiver in another process reads a frame later. Syphon announces
//! a frame only once its copy has run, so the wait changes nothing there.
//!
//! The tests run one at a time ([`SERIAL`]): on WARP, four or more devices
//! rendering in parallel while D3D11 devices are created for them crash
//! inside WARP's own threads (M1 log, 2026-10-17); one or two do not, and
//! hardware drivers do not. The app has one device. On macOS a Share In
//! with an empty name follows the newest server of any test, which is one
//! more reason.
#![cfg(any(windows, target_os = "macos"))]

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_interop::readback::read_rgba8;
use serde_json::{Value, json};

/// Long enough for WARP, which draws slowly.
const TIMEOUT: Duration = Duration::from_secs(30);

fn no_gpu(what: &str) {
    let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "LSR_REQUIRE_GPU is set but there is no {what}");
    eprintln!("skipping: no {what}");
}

/// The backend sharing needs: DX12 for Spout, Metal for Syphon.
const BACKEND: wgpu::Backends = if cfg!(windows) {
    wgpu::Backends::DX12
} else {
    wgpu::Backends::METAL
};

/// A device on [`BACKEND`], from the tests' usual instance.
fn share_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = lsr_gpu::primary_instance();
    let adapters = pollster::block_on(instance.enumerate_adapters(BACKEND));
    // Prefer a hardware adapter; CI has only WARP.
    let adapter = adapters
        .into_iter()
        .min_by_key(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn engine_on(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> Engine {
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    Engine::new(
        device.clone(),
        queue.clone(),
        reg,
        lib,
        EngineConfig {
            width: w,
            height: h,
            format,
        },
    )
}

static SERIAL: Mutex<()> = Mutex::new(());

/// Held for a whole test and dropped last (it is the first local), after
/// the test's engines: it waits for the Share workers to finish tearing
/// down, so the process never exits under them (macOS traps at exit).
struct Serial {
    _lock: MutexGuard<'static, ()>,
}

impl Drop for Serial {
    fn drop(&mut self) {
        let done = lsr_ops_img::share_wait_for_workers(std::time::Duration::from_secs(60));
        // A failing test is already unwinding; a second panic would abort.
        assert!(
            done || std::thread::panicking(),
            "Share workers still running 60 s after the test"
        );
    }
}

fn serial() -> Serial {
    Serial {
        _lock: SERIAL.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

macro_rules! device_or_skip {
    () => {
        match share_device() {
            Some(d) => d,
            None => {
                no_gpu("DX12 or Metal adapter");
                return;
            }
        }
    };
}

/// A sender name no other test, process or Spout/Syphon app uses.
fn unique(tag: &str) -> String {
    format!("lsr-test-{}-{tag}", std::process::id())
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
    e.edit("add", edits).unwrap();
    id
}

fn connect(e: &mut Engine, from: NodeId, to: NodeId) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, "in")),
        }],
    )
    .unwrap();
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

/// Cooks `nodes` until `done` holds.
fn cook_until(e: &mut Engine, nodes: &[NodeId], what: &str, mut done: impl FnMut(&Engine) -> bool) {
    let start = Instant::now();
    let mut t = 0.0;
    while !done(e) {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        e.cook(nodes, t).unwrap();
        t += 1.0 / 60.0;
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Waits until everything submitted has run on the GPU, which includes
/// Share Out's D3D11 copy on Windows (the queue waits on its fence).
fn idle(e: &Engine) {
    e.device()
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
}

fn published(e: &Engine, node: NodeId) -> Option<&str> {
    e.status(node)?
        .strip_prefix("Published as \"")?
        .strip_suffix('"')
}

fn receiving(e: &Engine, node: NodeId) -> bool {
    e.status(node).is_some_and(|s| s.starts_with("Receiving"))
}

/// Publishes `noise` through a Share Out called `name`, until it is
/// published and its first copy has run.
fn publish(e: &mut Engine, source: NodeId, name: &str) -> NodeId {
    let out = add(e, "img.share_out", &[("name", json!(name))]);
    connect(e, source, out);
    cook_until(e, &[out], "the sender", |e| {
        if let Some(err) = e.error(out) {
            panic!("share_out: {err}");
        }
        published(e, out) == Some(name)
    });
    cook_until(e, &[out], "the hand-off", |e| !e.pending());
    idle(e);
    out
}

fn pixels(e: &Engine, node: NodeId) -> Vec<u8> {
    read_rgba8(
        e.device(),
        e.queue(),
        e.result(node).expect("node has a result"),
    )
    .unwrap()
}

fn assert_close(a: &[u8], b: &[u8], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: sizes");
    let worst = a
        .iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "{what}: pixels differ by up to {worst} levels");
}

#[test]
fn loopback_in_one_engine_matches_pixels() {
    let _serial = serial();
    let (device, queue) = device_or_skip!();
    let mut e = engine_on(&device, &queue, 48, 24, wgpu::TextureFormat::Rgba8Unorm);
    let noise = add(&mut e, "img.noise", &[("scale", json!(9.0))]);
    let name = unique("pixels");
    publish(&mut e, noise, &name);
    assert!(lsr_ops_img::share_senders().contains(&name));

    let inp = add(&mut e, "img.share_in", &[("name", json!(name))]);
    cook_until(&mut e, &[inp], "the first frame", |e| receiving(e, inp));
    assert_eq!(e.error(inp), None);
    let got = e.result(inp).unwrap();
    assert_eq!(
        (got.width(), got.height()),
        (48, 24),
        "output follows the sender"
    );
    assert_eq!(
        e.status(inp),
        Some(format!("Receiving \"{name}\" (48×24)").as_str())
    );
    assert_close(&pixels(&e, noise), &pixels(&e, inp), "loopback");
}

/// Share Out in a float engine, Share In in a second (8-bit) engine on the
/// same device: the cook converts to 8-bit BGRA on the GPU.
#[test]
fn loopback_between_engines_converts_formats() {
    let _serial = serial();
    let (device, queue) = device_or_skip!();
    let mut a = engine_on(&device, &queue, 40, 30, wgpu::TextureFormat::Rgba16Float);
    let mut b = engine_on(&device, &queue, 40, 30, wgpu::TextureFormat::Rgba8Unorm);
    let card = [("scale", json!(6.0)), ("octaves", json!(4))];
    let src = add(&mut a, "img.noise", &card);
    let reference = add(&mut b, "img.noise", &card);
    let name = unique("engines");
    publish(&mut a, src, &name);

    let inp = add(&mut b, "img.share_in", &[("name", json!(name))]);
    cook_until(&mut b, &[reference, inp], "the first frame", |e| {
        receiving(e, inp)
    });
    assert_close(
        &pixels(&b, reference),
        &pixels(&b, inp),
        "float engine to 8-bit",
    );
}

#[test]
fn sender_goes_when_disabled_or_removed() {
    let _serial = serial();
    let (device, queue) = device_or_skip!();
    let mut e = engine_on(&device, &queue, 16, 16, wgpu::TextureFormat::Rgba8Unorm);
    let noise = add(&mut e, "img.noise", &[]);
    let name = unique("remove");
    let out = publish(&mut e, noise, &name);
    let listed = |name: &str| lsr_ops_img::share_senders().iter().any(|n| n == name);
    assert!(listed(&name));

    set(&mut e, out, "enabled", json!(false));
    cook_until(&mut e, &[out], "the sender to go", |_| !listed(&name));
    assert_eq!(e.status(out), Some("Disabled: not published"));
    assert!(e.result(out).is_some(), "still passes its input through");

    set(&mut e, out, "enabled", json!(true));
    cook_until(&mut e, &[out], "the sender to return", |e| {
        published(e, out) == Some(name.as_str()) && listed(&name)
    });

    e.edit("remove", [Edit::RemoveNode { id: out }]).unwrap();
    cook_until(&mut e, &[noise], "the removed node's sender to go", |_| {
        !listed(&name)
    });
}

#[test]
fn share_in_follows_a_resize() {
    let _serial = serial();
    let (device, queue) = device_or_skip!();
    let mut e = engine_on(&device, &queue, 40, 20, wgpu::TextureFormat::Rgba8Unorm);
    let noise = add(&mut e, "img.noise", &[]);
    let name = unique("resize");
    let out = publish(&mut e, noise, &name);
    let inp = add(&mut e, "img.share_in", &[("name", json!(name))]);
    cook_until(&mut e, &[inp], "the first frame", |e| receiving(e, inp));
    assert_eq!(
        e.result(inp).map(|t| (t.width(), t.height())),
        Some((40, 20))
    );

    // The source changes size: Share Out resizes the sender.
    e.set_resolution(64, 36);
    e.cook(&[out], 1.0).unwrap();
    cook_until(&mut e, &[out], "the hand-off", |e| !e.pending());
    idle(&e);
    cook_until(&mut e, &[inp], "the new size", |e| {
        e.result(inp).map(|t| (t.width(), t.height())) == Some((64, 36))
    });
    // The first frame at the new size may be the scaled old one.
    e.cook(&[inp], 2.0).unwrap();
    assert_eq!(
        e.status(inp),
        Some(format!("Receiving \"{name}\" (64×36)").as_str())
    );
    assert_close(&pixels(&e, noise), &pixels(&e, inp), "after the resize");
}

#[test]
fn share_in_without_a_sender_is_black_and_quiet() {
    let _serial = serial();
    let (device, queue) = device_or_skip!();
    let mut e = engine_on(&device, &queue, 20, 10, wgpu::TextureFormat::Rgba8Unorm);
    let name = unique("missing");
    let inp = add(&mut e, "img.share_in", &[("name", json!(name))]);
    for frame in 0..20 {
        e.cook(&[inp], f64::from(frame) / 60.0).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(e.error(inp), None);
    assert_eq!(
        e.status(inp),
        Some(format!("Waiting for \"{name}\"").as_str())
    );
    let got = e.result(inp).unwrap();
    assert_eq!((got.width(), got.height()), (20, 10), "project size");
    assert!(pixels(&e, inp).iter().all(|&v| v == 0), "black");
}

#[test]
fn bad_names_are_node_errors() {
    let _serial = serial();
    let (device, queue) = device_or_skip!();
    let mut e = engine_on(&device, &queue, 16, 16, wgpu::TextureFormat::Rgba8Unorm);
    let noise = add(&mut e, "img.noise", &[]);
    // Spout names are 1-255 bytes without a backslash; Syphon's at most
    // 255 bytes without NUL (empty is allowed).
    let (bads, message) = if cfg!(windows) {
        (
            vec![String::new(), "a\\b".to_owned(), "x".repeat(300)],
            "invalid Spout sender name",
        )
    } else {
        (
            vec!["a\0b".to_owned(), "x".repeat(300)],
            "a Syphon server name is at most 255 bytes",
        )
    };
    for bad in bads {
        let out = add(&mut e, "img.share_out", &[("name", json!(bad))]);
        connect(&mut e, noise, out);
        let inp = add(&mut e, "img.share_in", &[("name", json!(bad))]);
        cook_until(&mut e, &[out, inp], "the errors", |e| {
            e.error(out).is_some() && (bad.is_empty() || e.error(inp).is_some())
        });
        assert!(
            e.error(out).unwrap().contains(message),
            "{:?}",
            e.error(out)
        );
        e.edit(
            "remove",
            [Edit::RemoveNode { id: out }, Edit::RemoveNode { id: inp }],
        )
        .unwrap();
    }
}
