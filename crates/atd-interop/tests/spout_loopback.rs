//! Spout loopback in one process: a sender on one wgpu device, a receiver
//! on another (each with its own instance and D3D11 device, as two
//! processes would have), through the machine-wide Spout shared memory.
#![cfg(windows)]

use std::sync::Mutex;
use std::time::Instant;

use atd_interop::dx12::SharedTexture;
use atd_interop::readback::{read_rgba8, test_pattern, write_rgba8};
use atd_interop::spout::{Received, SpoutReceiver, SpoutSender};

/// The tests share the sender list and the active-sender name, so they run
/// one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// Skips without a GPU, unless `ATD_REQUIRE_GPU` is set (CI on Windows and
/// macOS), where a missing adapter is a failure.
fn no_gpu(what: &str) {
    let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "ATD_REQUIRE_GPU is set but there is no {what}");
    eprintln!("skipping: no {what}");
}

/// A device on its own DX12 instance, like a second process would have.
fn dx12_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

struct Pair {
    dev_a: wgpu::Device,
    queue_a: wgpu::Queue,
    dev_b: wgpu::Device,
    queue_b: wgpu::Queue,
}

fn pair() -> Option<Pair> {
    let Some((dev_a, queue_a)) = dx12_device() else {
        no_gpu("DX12 adapter");
        return None;
    };
    let (dev_b, queue_b) = dx12_device().unwrap();
    Some(Pair {
        dev_a,
        queue_a,
        dev_b,
        queue_b,
    })
}

/// A sender name no other test, process or Spout app uses.
fn unique(tag: &str) -> String {
    format!("atd-test-{}-{tag}", std::process::id())
}

/// Waits until everything submitted to `queue` has run. After a send, the
/// sender's queue waits on the fence D3D11 signals after its copy, so this
/// also waits for the copy into the Spout texture.
fn finish(device: &wgpu::Device, queue: &wgpu::Queue) {
    queue.submit([]);
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
}

/// Asks the receiver for the sender's size and format, then makes a
/// matching target.
fn target_for(p: &Pair, receiver: &mut SpoutReceiver) -> SharedTexture {
    let probe =
        SharedTexture::create(&p.dev_b, 1, 1, wgpu::TextureFormat::Rgba8Unorm, None).unwrap();
    match receiver.receive_into_shared(&p.queue_b, &probe).unwrap() {
        Received::Resize {
            width,
            height,
            format,
        } => SharedTexture::create(&p.dev_b, width, height, format, None).unwrap(),
        other => panic!("expected Resize, got {other:?}"),
    }
}

/// Sends a test pattern and waits for the sender's copy to finish. Spout
/// orders nothing between two D3D11 devices beyond `Flush`; hardware
/// drivers sync legacy shared textures implicitly, WARP (CI) does not, and
/// a receiver in another process would normally read a frame later.
fn send(
    p: &Pair,
    sender: &mut SpoutSender,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    seed: u8,
) -> Vec<u8> {
    let source = SharedTexture::create(&p.dev_a, width, height, format, None).unwrap();
    let pattern = test_pattern(width, height, seed);
    write_rgba8(&p.queue_a, &source.texture, &pattern);
    assert!(sender.send_wgpu(&p.queue_a, &source).unwrap());
    finish(&p.dev_a, &p.queue_a);
    pattern
}

#[test]
fn loopback_pixels_match() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pair() else { return };
    let (w, h) = (96, 40);
    for format in atd_interop::SHAREABLE_FORMATS {
        let mut sender = SpoutSender::new(&p.dev_a, &unique("pixels"), w, h, format).unwrap();
        let first = send(&p, &mut sender, w, h, format, 7);

        let mut receiver = SpoutReceiver::connect(&p.dev_b, Some(sender.name())).unwrap();
        let target = target_for(&p, &mut receiver);
        assert_eq!(target.texture.format(), format);
        assert_eq!(
            receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
            Received::NewFrame
        );
        assert_eq!(
            read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap(),
            first,
            "{format:?}: first frame"
        );
        assert_eq!(
            receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
            Received::NoNewFrame,
            "{format:?}: no new frame yet"
        );

        let second = send(&p, &mut sender, w, h, format, 99);
        assert_eq!(
            receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
            Received::NewFrame
        );
        assert_eq!(
            read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap(),
            second,
            "{format:?}: second frame"
        );
    }
}

#[test]
fn loopback_resize_mid_stream() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pair() else { return };
    let bgra = wgpu::TextureFormat::Bgra8Unorm;
    let mut sender = SpoutSender::new(&p.dev_a, &unique("resize"), 64, 32, bgra).unwrap();
    send(&p, &mut sender, 64, 32, bgra, 1);
    let mut receiver = SpoutReceiver::connect(&p.dev_b, Some(sender.name())).unwrap();
    let target = target_for(&p, &mut receiver);
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NewFrame
    );

    // A source of another size: `send_wgpu` resizes the sender.
    let pattern = send(&p, &mut sender, 80, 50, bgra, 2);
    assert_eq!(sender.size(), (80, 50));
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::Resize {
            width: 80,
            height: 50,
            format: bgra
        }
    );
    let target = target_for(&p, &mut receiver);
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NewFrame
    );
    assert_eq!(
        read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap(),
        pattern
    );

    // An explicit resize that also changes the format.
    let rgba = wgpu::TextureFormat::Rgba8Unorm;
    sender.resize(40, 24, rgba).unwrap();
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::Resize {
            width: 40,
            height: 24,
            format: rgba
        }
    );
    let pattern = send(&p, &mut sender, 40, 24, rgba, 3);
    let target = target_for(&p, &mut receiver);
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NewFrame
    );
    assert_eq!(
        read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap(),
        pattern
    );
}

#[test]
fn sender_drop_removes_name() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pair() else { return };
    let fmt = wgpu::TextureFormat::Bgra8Unorm;
    let mut sender = SpoutSender::new(&p.dev_a, &unique("drop"), 32, 32, fmt).unwrap();
    let name = sender.name().to_owned();
    assert!(SpoutReceiver::list_senders().contains(&name));
    assert_eq!(
        SpoutReceiver::active_sender().as_deref(),
        Some(name.as_str()),
        "the newest sender is active"
    );

    // A receiver following the active sender finds it.
    let pattern = send(&p, &mut sender, 32, 32, fmt, 5);
    let mut receiver = SpoutReceiver::connect(&p.dev_b, None).unwrap();
    let target = target_for(&p, &mut receiver);
    assert_eq!(receiver.sender_name().as_deref(), Some(name.as_str()));
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NewFrame
    );
    assert_eq!(
        read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap(),
        pattern
    );

    drop(sender);
    assert!(!SpoutReceiver::list_senders().contains(&name));
    assert_ne!(
        SpoutReceiver::active_sender().as_deref(),
        Some(name.as_str())
    );
    // The receiver notices, and keeps working once a sender of that name returns.
    let mut named = SpoutReceiver::connect(&p.dev_b, Some(&name)).unwrap();
    assert_eq!(
        named.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NoSender
    );
    let mut again = SpoutSender::new(&p.dev_a, &name, 32, 32, fmt).unwrap();
    assert_eq!(again.name(), name, "a dropped sender's name is free");
    let pattern = send(&p, &mut again, 32, 32, fmt, 6);
    assert_eq!(
        named.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NewFrame
    );
    assert_eq!(
        read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap(),
        pattern
    );
    drop(again);
    assert_eq!(
        receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
        Received::NoSender
    );
}

#[test]
fn second_sender_with_same_name_gets_suffix() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pair() else { return };
    let fmt = wgpu::TextureFormat::Bgra8Unorm;
    let base = unique("dup");
    let a = SpoutSender::new(&p.dev_a, &base, 16, 16, fmt).unwrap();
    let b = SpoutSender::new(&p.dev_a, &base, 16, 16, fmt).unwrap();
    let c = SpoutSender::new(&p.dev_a, &base, 16, 16, fmt).unwrap();
    assert_eq!(a.name(), base);
    assert_eq!(b.name(), format!("{base}_1"));
    assert_eq!(c.name(), format!("{base}_2"));
    let names = SpoutReceiver::list_senders();
    assert!(
        [&a, &b, &c]
            .iter()
            .all(|s| names.iter().any(|n| n == s.name()))
    );
    drop(b);
    let d = SpoutSender::new(&p.dev_a, &base, 16, 16, fmt).unwrap();
    assert_eq!(d.name(), format!("{base}_1"), "the freed suffix is reused");
}

#[test]
fn rejects_bad_names_and_formats() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some((dev, _)) = dx12_device() else {
        return;
    };
    let fmt = wgpu::TextureFormat::Bgra8Unorm;
    assert!(SpoutSender::new(&dev, "", 16, 16, fmt).is_err());
    assert!(SpoutSender::new(&dev, &"x".repeat(256), 16, 16, fmt).is_err());
    assert!(matches!(
        SpoutSender::new(
            &dev,
            &unique("fmt"),
            16,
            16,
            wgpu::TextureFormat::Rgba16Float
        ),
        Err(atd_interop::InteropError::UnsupportedFormat(_))
    ));
}

/// Per-frame cost at 1080p and 4K. Run with
/// `cargo test -p atd-interop --release --test spout_loopback -- --ignored --nocapture`.
#[test]
#[ignore = "timing, run by hand"]
fn loopback_timing() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pair() else { return };
    let fmt = wgpu::TextureFormat::Bgra8Unorm;
    const FRAMES: usize = 300;
    let ms = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (
            v.iter().sum::<f64>() / v.len() as f64,
            v[v.len() / 2],
            v[v.len() * 95 / 100],
        )
    };
    println!(
        "| size | send call ms (mean/p50/p95) | receive call ms (mean/p50/p95) | round trip ms (mean/p50/p95) | readback ms (mean/p50/p95) |"
    );
    println!("|---|---|---|---|---|");
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let mut sender = SpoutSender::new(&p.dev_a, &unique("timing"), w, h, fmt).unwrap();
        let source = SharedTexture::create(&p.dev_a, w, h, fmt, None).unwrap();
        write_rgba8(&p.queue_a, &source.texture, &test_pattern(w, h, 1));
        sender.send_wgpu(&p.queue_a, &source).unwrap();
        finish(&p.dev_a, &p.queue_a);
        let mut receiver = SpoutReceiver::connect(&p.dev_b, Some(sender.name())).unwrap();
        let target = target_for(&p, &mut receiver);
        let (mut send_ms, mut recv_ms, mut trip_ms, mut read_ms) = (vec![], vec![], vec![], vec![]);
        for _ in 0..FRAMES {
            let t0 = Instant::now();
            assert!(sender.send_wgpu(&p.queue_a, &source).unwrap());
            let t1 = Instant::now();
            assert_eq!(
                receiver.receive_into_shared(&p.queue_b, &target).unwrap(),
                Received::NewFrame
            );
            let t2 = Instant::now();
            // Until device B's queue has passed the fence its D3D11 device
            // signals after copying: the whole chain has run on the GPU.
            finish(&p.dev_b, &p.queue_b);
            let t3 = Instant::now();
            finish(&p.dev_a, &p.queue_a);
            send_ms.push((t1 - t0).as_secs_f64() * 1e3);
            recv_ms.push((t2 - t1).as_secs_f64() * 1e3);
            trip_ms.push((t3 - t0).as_secs_f64() * 1e3);
        }
        for _ in 0..30 {
            let t0 = Instant::now();
            read_rgba8(&p.dev_b, &p.queue_b, &target.texture).unwrap();
            read_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        }
        let f = |(a, b, c): (f64, f64, f64)| format!("{a:.3} / {b:.3} / {c:.3}");
        println!(
            "| {w}x{h} | {} | {} | {} | {} |",
            f(ms(&mut send_ms)),
            f(ms(&mut recv_ms)),
            f(ms(&mut trip_ms)),
            f(ms(&mut read_ms))
        );
    }
}
