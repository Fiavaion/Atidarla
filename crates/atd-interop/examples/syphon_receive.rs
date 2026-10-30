//! Syphon client: receives a server's frames, reads each one back and
//! checks it against the stamped test pattern (`common/stamped.rs`).
//!
//! `cargo run -p atd-interop --example syphon_receive [name|-] [seconds]`
//!
//! `-` (the default) takes the newest server. The main thread runs its run
//! loop between frames, which Syphon's directory needs to hear about other
//! processes' servers (see `atd_interop::syphon`).

#[cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "common/stamped.rs"]
mod stamped;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Syphon is macOS-only");
}

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Instant;

    use atd_interop::metal::SharedSurface;
    use atd_interop::readback::read_rgba8;
    use atd_interop::syphon::{
        Received, SYPHON_FORMAT, SyphonReceiver, list_servers, run_main_loop,
    };

    let mut args = std::env::args().skip(1);
    let name = args.next().filter(|n| !n.is_empty() && n != "-");
    let seconds: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("no Metal adapter");
    println!("adapter: {}", adapter.get_info().name);
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");

    // Loading the framework starts the directory, which asks running
    // servers to announce themselves; let it hear them.
    atd_interop::syphon::load().expect("Syphon framework");
    run_main_loop(0.5);
    println!("servers: {:?}", list_servers().expect("servers"));
    let mut receiver = SyphonReceiver::connect(&device, &queue, name.as_deref()).expect("receiver");
    let mut target = SharedSurface::create(&device, 1, 1, SYPHON_FORMAT).unwrap();

    let start = Instant::now();
    let mut last_status = None;
    let (mut frames, mut ok, mut bad, mut gaps) = (0u64, 0u64, 0u64, 0u64);
    let mut last_frame: Option<u32> = None;
    let (mut age_ms, mut receive_ms) = (Vec::new(), Vec::new());
    while start.elapsed().as_secs_f32() < seconds {
        let called = Instant::now();
        let status = receiver.receive_into(&target.texture).expect("receive");
        let call_ms = called.elapsed().as_secs_f64() * 1e3;
        match status {
            Received::Resize {
                width,
                height,
                format,
            } => {
                println!(
                    "server {:?}: {width}x{height} {format:?}",
                    receiver.server().map(|s| s.label().to_owned())
                );
                target = SharedSurface::create(&device, width, height, format).unwrap();
                continue;
            }
            Received::NewFrame => {
                frames += 1;
                receive_ms.push(call_ms);
                let pixels = read_rgba8(&device, &queue, &target.texture).unwrap();
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
            Received::NoSender | Received::NoNewFrame => {}
        }
        if last_status != Some(status) && status == Received::NoSender {
            println!("waiting for a server...");
        }
        last_status = Some(status);
        run_main_loop(0.001);
    }
    println!("received {frames} frames");
    println!("verified: {ok} distinct frames match, {bad} bad, {gaps} skipped by number");
    println!(
        "receive_into ms mean / p50 / p95 / max: {}",
        stamped::summary(&mut receive_ms)
    );
    println!(
        "stamp-to-pixels-on-CPU ms (includes readback): {}",
        stamped::summary(&mut age_ms)
    );
    if bad > 0 || ok == 0 {
        std::process::exit(1);
    }
}
