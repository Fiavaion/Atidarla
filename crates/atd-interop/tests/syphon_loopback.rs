//! Syphon loopback in one process: a server on one wgpu device, a client on
//! another (each with its own instance, as two processes would have),
//! through the official Syphon framework. The client finds the server in
//! this process's own server list; `syphon_directory.rs` checks the
//! directory, which needs the main run loop.
#![cfg(target_os = "macos")]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use atd_interop::metal::SharedSurface;
use atd_interop::readback::{read_rgba8, test_pattern, write_rgba8};
use atd_interop::syphon::{
    Received, SYPHON_FORMAT, ServerInfo, SyphonReceiver, SyphonServer, list_servers,
};

/// Receivers that follow the newest server see every test's servers, so
/// the tests run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Skips without a GPU, unless `ATD_REQUIRE_GPU` is set (CI on Windows and
/// macOS), where a missing adapter is a failure.
fn no_gpu(what: &str) {
    let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "ATD_REQUIRE_GPU is set but there is no {what}");
    eprintln!("skipping: no {what}");
}

fn metal_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

struct Side {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn pair() -> Option<(Side, Side)> {
    let Some((device, queue)) = metal_device() else {
        no_gpu("Metal adapter");
        return None;
    };
    let (dev_b, queue_b) = metal_device().unwrap();
    Some((
        Side { device, queue },
        Side {
            device: dev_b,
            queue: queue_b,
        },
    ))
}

/// A server name no other test, process or Syphon app uses.
fn unique(tag: &str) -> String {
    format!("atd-test-{}-{tag}", std::process::id())
}

fn finish(side: &Side) {
    side.queue.submit([]);
    side.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
}

fn source(
    side: &Side,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    pixels: &[u8],
) -> SharedSurface {
    let s = SharedSurface::create(&side.device, w, h, format).unwrap();
    write_rgba8(&side.queue, &s.texture, pixels);
    s
}

/// Polls until the receiver reports something other than `NoNewFrame` or
/// `NoSender` (Syphon announces a frame asynchronously, once its copy has
/// run on the GPU), or 3 s pass.
fn wait(side: &Side, receiver: &mut SyphonReceiver, target: &wgpu::Texture) -> Received {
    let start = Instant::now();
    loop {
        let got = receiver.receive_into(target).unwrap();
        if !matches!(got, Received::NoNewFrame | Received::NoSender)
            || start.elapsed() > Duration::from_secs(3)
        {
            finish(side);
            return got;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A landing texture of the size the receiver asks for.
fn target_for(side: &Side, receiver: &mut SyphonReceiver) -> SharedSurface {
    let probe = SharedSurface::create(&side.device, 1, 1, SYPHON_FORMAT).unwrap();
    match wait(side, receiver, &probe.texture) {
        Received::Resize {
            width,
            height,
            format,
        } => {
            assert_eq!(format, SYPHON_FORMAT);
            SharedSurface::create(&side.device, width, height, format).unwrap()
        }
        other => panic!("expected a resize, got {other:?}"),
    }
}

/// Publishes `pixels` from `from` and receives them on `to`, byte for byte.
fn send_and_check(from: &Side, to: &Side, tag: &str) {
    let (w, h) = (96, 40);
    let name = unique(tag);
    let pattern = test_pattern(w, h, 5);
    let src = source(from, w, h, wgpu::TextureFormat::Bgra8Unorm, &pattern);
    let mut server = SyphonServer::new(&from.device, &from.queue, &name).unwrap();
    assert_eq!(server.info().name, name);
    let mut receiver = SyphonReceiver::connect(&to.device, &to.queue, Some(&name)).unwrap();
    let probe = SharedSurface::create(&to.device, 1, 1, SYPHON_FORMAT).unwrap();
    assert_eq!(
        receiver.receive_into(&probe.texture).unwrap(),
        Received::NoNewFrame
    );
    server.publish(&src.texture).unwrap();
    finish(from);
    let target = target_for(to, &mut receiver);
    assert_eq!((target.texture.width(), target.texture.height()), (w, h));
    assert_eq!(wait(to, &mut receiver, &target.texture), Received::NewFrame);
    assert_eq!(
        receiver.server().map(|s| s.name.as_str()),
        Some(name.as_str())
    );
    assert_eq!(
        read_rgba8(&to.device, &to.queue, &target.texture).unwrap(),
        pattern,
        "{tag}: pixels"
    );
    assert!(server.has_clients(), "{tag}: the server counts the client");
}

#[test]
fn pixels_cross_in_both_directions() {
    let _serial = serial();
    let Some((a, b)) = pair() else { return };
    send_and_check(&a, &b, "a-to-b");
    send_and_check(&b, &a, "b-to-a");
}

#[test]
fn rgba_sources_arrive_as_bgra() {
    let _serial = serial();
    let Some((a, b)) = pair() else { return };
    let (w, h) = (64, 32);
    let name = unique("rgba");
    let pattern = test_pattern(w, h, 9);
    let src = source(&a, w, h, wgpu::TextureFormat::Rgba8Unorm, &pattern);
    let mut server = SyphonServer::new(&a.device, &a.queue, &name).unwrap();
    let mut receiver = SyphonReceiver::connect(&b.device, &b.queue, Some(&name)).unwrap();
    let probe = SharedSurface::create(&b.device, 1, 1, SYPHON_FORMAT).unwrap();
    assert_eq!(
        receiver.receive_into(&probe.texture).unwrap(),
        Received::NoNewFrame
    );
    server.publish(&src.texture).unwrap();
    finish(&a);
    let target = target_for(&b, &mut receiver);
    assert_eq!(wait(&b, &mut receiver, &target.texture), Received::NewFrame);
    let mut got = read_rgba8(&b.device, &b.queue, &target.texture).unwrap();
    for px in got.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    assert_eq!(got, pattern);
}

#[test]
fn new_frames_resizes_and_a_server_that_goes() {
    let _serial = serial();
    let Some((a, b)) = pair() else { return };
    let name = unique("stream");
    let first = test_pattern(40, 20, 1);
    let src = source(&a, 40, 20, wgpu::TextureFormat::Bgra8Unorm, &first);
    let mut server = SyphonServer::new(&a.device, &a.queue, &name).unwrap();
    let mut receiver = SyphonReceiver::connect(&b.device, &b.queue, Some(&name)).unwrap();

    // Running, but nothing published yet.
    let probe = SharedSurface::create(&b.device, 1, 1, SYPHON_FORMAT).unwrap();
    assert_eq!(
        receiver.receive_into(&probe.texture).unwrap(),
        Received::NoNewFrame
    );

    server.publish(&src.texture).unwrap();
    finish(&a);
    let target = target_for(&b, &mut receiver);
    assert_eq!(wait(&b, &mut receiver, &target.texture), Received::NewFrame);
    assert_eq!(
        read_rgba8(&b.device, &b.queue, &target.texture).unwrap(),
        first
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        receiver.receive_into(&target.texture).unwrap(),
        Received::NoNewFrame,
        "no new frame until the server publishes again"
    );

    let second = test_pattern(40, 20, 2);
    write_rgba8(&a.queue, &src.texture, &second);
    server.publish(&src.texture).unwrap();
    finish(&a);
    assert_eq!(wait(&b, &mut receiver, &target.texture), Received::NewFrame);
    assert_eq!(
        read_rgba8(&b.device, &b.queue, &target.texture).unwrap(),
        second
    );

    // A resize mid-stream.
    let big = test_pattern(64, 36, 3);
    let src2 = source(&a, 64, 36, wgpu::TextureFormat::Bgra8Unorm, &big);
    server.publish(&src2.texture).unwrap();
    finish(&a);
    assert_eq!(
        wait(&b, &mut receiver, &target.texture),
        Received::Resize {
            width: 64,
            height: 36,
            format: SYPHON_FORMAT
        }
    );
    let target2 = SharedSurface::create(&b.device, 64, 36, SYPHON_FORMAT).unwrap();
    assert_eq!(
        wait(&b, &mut receiver, &target2.texture),
        Received::NewFrame
    );
    assert_eq!(
        read_rgba8(&b.device, &b.queue, &target2.texture).unwrap(),
        big
    );

    // The server goes: the receiver sees it, and finds it again when a
    // server of that name comes back.
    drop(server);
    assert!(!list_servers().unwrap().iter().any(|s| s.name == name));
    let start = Instant::now();
    while receiver.receive_into(&target2.texture).unwrap() != Received::NoSender {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "server drop not seen"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(receiver.server().is_none());
    let mut again = SyphonServer::new(&a.device, &a.queue, &name).unwrap();
    again.publish(&src2.texture).unwrap();
    finish(&a);
    assert_eq!(
        wait(&b, &mut receiver, &target2.texture),
        Received::NewFrame
    );
    assert_eq!(
        read_rgba8(&b.device, &b.queue, &target2.texture).unwrap(),
        big
    );
}

#[test]
fn unnamed_receivers_take_the_newest_server() {
    let _serial = serial();
    let Some((a, b)) = pair() else { return };
    let old = unique("old");
    let new = unique("new");
    let src = source(
        &a,
        8,
        8,
        wgpu::TextureFormat::Bgra8Unorm,
        &test_pattern(8, 8, 0),
    );
    let mut s_old = SyphonServer::new(&a.device, &a.queue, &old).unwrap();
    let mut s_new = SyphonServer::new(&a.device, &a.queue, &new).unwrap();
    s_old.publish(&src.texture).unwrap();
    s_new.publish(&src.texture).unwrap();
    finish(&a);
    let names: Vec<String> = list_servers()
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    let (i_old, i_new) = (
        names.iter().position(|n| *n == old).unwrap(),
        names.iter().position(|n| *n == new).unwrap(),
    );
    assert!(i_old < i_new, "oldest first: {names:?}");
    let mut receiver = SyphonReceiver::connect(&b.device, &b.queue, None).unwrap();
    let target = target_for(&b, &mut receiver);
    assert_eq!(wait(&b, &mut receiver, &target.texture), Received::NewFrame);
    assert_eq!(
        receiver.server().map(|s| s.name.as_str()),
        Some(new.as_str())
    );
}

#[test]
fn names_match_as_syphon_apps_list_them() {
    let info = ServerInfo {
        name: "Main".into(),
        app_name: "Atidarla".into(),
        uuid: "x".into(),
    };
    assert!(info.matches("Main"));
    assert!(info.matches("Atidarla - Main"));
    assert!(!info.matches("Atidarla"));
    assert!(!info.matches("Other - Main"));
    let unnamed = ServerInfo {
        name: String::new(),
        ..info
    };
    assert_eq!(unnamed.label(), "Atidarla");
    assert!(unnamed.matches("Atidarla"));
}

#[test]
fn bad_names_are_errors() {
    let Some((a, _b)) = pair() else { return };
    for bad in ["a\0b".to_owned(), "x".repeat(256)] {
        assert!(SyphonServer::new(&a.device, &a.queue, &bad).is_err());
        assert!(SyphonReceiver::connect(&a.device, &a.queue, Some(&bad)).is_err());
    }
}
