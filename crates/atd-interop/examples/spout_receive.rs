//! Spout receiver: lists the senders, receives one into a wgpu texture and
//! prints each new frame's size and a checksum (read back, about twice a
//! second).
//!
//! `cargo run -p atd-interop --example spout_receive [name] [seconds]`
//!
//! Without a name it follows the active sender. Default 30 s.

#[cfg(not(windows))]
fn main() {
    eprintln!("Spout is Windows-only");
}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use atd_interop::dx12::SharedTexture;
    use atd_interop::readback::read_rgba8;
    use atd_interop::spout::{Received, SpoutReceiver};

    let mut args = std::env::args().skip(1);
    let name = args.next().filter(|n| !n.is_empty());
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
    while start.elapsed().as_secs_f32() < seconds {
        let status = receiver
            .receive_into_shared(&queue, &target)
            .expect("receive");
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
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("received {frames} frames");
}
