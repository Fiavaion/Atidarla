//! Spout receiver: lists the senders, receives one into a wgpu texture and
//! prints each new frame's size and a checksum (read back, about twice a
//! second).
//!
//! `cargo run -p lsr-interop --example spout_receive [name] [seconds] [--verify]`
//!
//! Without a name (or with "" or "-") it follows the active sender. Default 30 s.
//! `--verify` (used by the SDK interop check, `docs/spikes/spout-interop/`)
//! reads back every new frame, checks it against the stamped test pattern
//! (`common/stamped.rs`) and prints counts and latencies at the end.

#[cfg(windows)]
#[allow(dead_code)]
#[path = "common/stamped.rs"]
mod stamped;

#[cfg(not(windows))]
fn main() {
    eprintln!("Spout is Windows-only");
}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use lsr_interop::dx12::SharedTexture;
    use lsr_interop::readback::read_rgba8;
    use lsr_interop::spout::{Received, SpoutReceiver};

    let (flags, args): (Vec<String>, Vec<String>) =
        std::env::args().skip(1).partition(|a| a.starts_with("--"));
    let verify = flags.iter().any(|f| f == "--verify");
    let mut args = args.into_iter();
    let name = args.next().filter(|n| !n.is_empty() && n != "-");
    let seconds: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("no DX12 adapter");
    println!("adapter: {}", adapter.get_info().name);
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");

    println!("senders: {:?}", SpoutReceiver::list_senders());
    println!("active: {:?}", SpoutReceiver::active_sender());
    let mut receiver = SpoutReceiver::connect(&device, name.as_deref()).expect("receiver");
    let mut target =
        SharedTexture::create(&device, 1, 1, wgpu::TextureFormat::Bgra8Unorm, None).unwrap();

    let start = Instant::now();
    let mut last_print = start - Duration::from_secs(1);
    let mut last_status = None;
    let mut frames = 0u64;
    // --verify statistics.
    let (mut ok, mut bad, mut gaps) = (0u64, 0u64, 0u64);
    let mut last_frame: Option<u32> = None;
    let (mut age_ms, mut readback_ms, mut receive_ms) = (Vec::new(), Vec::new(), Vec::new());
    while start.elapsed().as_secs_f32() < seconds {
        let called = Instant::now();
        let status = receiver
            .receive_into_shared(&queue, &target)
            .expect("receive");
        let call_ms = called.elapsed().as_secs_f64() * 1e3;
        match status {
            Received::Resize {
                width,
                height,
                format,
            } => {
                println!(
                    "sender {:?}: {width}x{height} {format:?}",
                    receiver.sender_name().unwrap_or_default()
                );
                target = SharedTexture::create(&device, width, height, format, None).unwrap();
                continue;
            }
            Received::NewFrame if verify => {
                frames += 1;
                receive_ms.push(call_ms);
                let read_at = Instant::now();
                let pixels = read_rgba8(&device, &queue, &target.texture).unwrap();
                readback_ms.push(read_at.elapsed().as_secs_f64() * 1e3);
                let size = target.texture.size();
                match stamped::check(&pixels, size.width, size.height) {
                    Ok(stamp) if last_frame == Some(stamp.frame) => {}
                    Ok(stamp) => {
                        ok += 1;
                        age_ms.push(stamp.age_ms);
                        if let Some(last) = last_frame {
                            gaps += u64::from(stamp.frame.saturating_sub(last + 1));
                        }
                        last_frame = Some(stamp.frame);
                    }
                    Err(e) => {
                        bad += 1;
                        if bad <= 5 {
                            println!("BAD: {e}");
                        }
                    }
                }
            }
            Received::NewFrame => {
                frames += 1;
                if last_print.elapsed() >= Duration::from_millis(500) {
                    last_print = Instant::now();
                    // Blocking readback: fine for an example, never on the
                    // render thread.
                    let pixels = read_rgba8(&device, &queue, &target.texture).unwrap();
                    // FNV-1a over the raw pixels.
                    let checksum = pixels.iter().fold(0x811c_9dc5u32, |h, &b| {
                        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
                    });
                    let size = target.texture.size();
                    println!(
                        "frame {frames}: {}x{} checksum {checksum:08x}",
                        size.width, size.height
                    );
                }
            }
            Received::NoSender | Received::NoNewFrame => {}
        }
        if last_status != Some(status) && status == Received::NoSender {
            println!("waiting for a sender...");
        }
        last_status = Some(status);
        let _ = device.poll(wgpu::PollType::Poll);
        std::thread::sleep(Duration::from_millis(if verify { 1 } else { 5 }));
    }
    println!("received {frames} frames");
    if verify {
        println!("verified: {ok} distinct frames match, {bad} bad, {gaps} skipped by number");
        println!(
            "receive_into_shared ms mean / p50 / p95 / max: {}",
            stamped::summary(&mut receive_ms)
        );
        println!(
            "stamp-to-pixels-on-CPU ms (includes readback): {}",
            stamped::summary(&mut age_ms)
        );
        println!(
            "readback ms (read_rgba8): {}",
            stamped::summary(&mut readback_ms)
        );
    }
}
