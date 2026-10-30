//! Syphon server: publishes the stamped test pattern (`common/stamped.rs`)
//! at 60 fps, a new seed every frame.
//!
//! `cargo run -p atd-interop --example syphon_send [name] [seconds] [width] [height] [--resize]`
//!
//! Defaults: "Atidarla Test", 30 s, 1280x720. `--resize` halves the size
//! half-way through. The main thread runs its run loop between frames, so
//! clients started later find the server. Open it in any Syphon client, or
//! check it with the `syphon_receive` example or the interop peers in
//! `docs/spikes/syphon-interop/`.

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
    use std::time::{Duration, Instant};

    use atd_interop::metal::SharedSurface;
    use atd_interop::readback::write_rgba8;
    use atd_interop::syphon::{SYPHON_FORMAT, SyphonServer, run_main_loop};

    let (flags, args): (Vec<String>, Vec<String>) =
        std::env::args().skip(1).partition(|a| a.starts_with("--"));
    let resize = flags.iter().any(|f| f == "--resize");
    let mut args = args.into_iter();
    let name = args.next().unwrap_or_else(|| "Atidarla Test".into());
    let seconds: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let width: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1280);
    let height: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(720);

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

    let mut server = SyphonServer::new(&device, &queue, &name).expect("server");
    println!("server {:?} ({})", server.info().name, server.info().uuid);
    let mut size = (width, height);
    let mut source = SharedSurface::create(&device, size.0, size.1, SYPHON_FORMAT).unwrap();

    let frame_time = Duration::from_secs_f64(1.0 / 60.0);
    let start = Instant::now();
    let mut frame = 0u32;
    let mut publish_ms = Vec::new();
    while start.elapsed().as_secs_f32() < seconds {
        if resize && size == (width, height) && start.elapsed().as_secs_f32() > seconds / 2.0 {
            size = (width / 2, height / 2);
            source = SharedSurface::create(&device, size.0, size.1, SYPHON_FORMAT).unwrap();
            println!("resized to {}x{}", size.0, size.1);
        }
        let pixels = stamped::frame_pixels(size.0, size.1, frame, (frame % 256) as u8);
        write_rgba8(&queue, &source.texture, &pixels);
        let called = Instant::now();
        server.publish(&source.texture).expect("publish");
        publish_ms.push(called.elapsed().as_secs_f64() * 1e3);
        let _ = device.poll(wgpu::PollType::Poll);
        frame += 1;
        // Wait on the main run loop, so that clients started later can
        // ask this server to announce itself (see `atd_interop::syphon`).
        if let Some(wait) = (frame_time * frame).checked_sub(start.elapsed()) {
            run_main_loop(wait.as_secs_f64());
        }
    }
    println!(
        "sent {frame} frames; clients attached at the end: {}",
        server.has_clients()
    );
    println!(
        "publish ms mean / p50 / p95 / max: {}",
        stamped::summary(&mut publish_ms)
    );
}
