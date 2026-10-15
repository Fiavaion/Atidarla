//! Spike (a) on macOS: a texture written on one wgpu device is read on an
//! independent device through an IOSurface looked up by ID, as a Syphon
//! client in another process would.
#![cfg(target_os = "macos")]

use atd_interop::metal::{SharedSurface, import, lookup};
use atd_interop::readback::{read_rgba8, test_pattern, write_rgba8};

const W: u32 = 96;
const H: u32 = 40;

fn metal_device() -> Option<(wgpu::Device, wgpu::Queue, String)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let name = adapter.get_info().name;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some((device, queue, name))
}

fn finish(device: &wgpu::Device, queue: &wgpu::Queue) {
    queue.submit([]);
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
}

#[test]
fn iosurface_texture_crosses_devices() {
    let Some((dev_a, queue_a, name)) = metal_device() else {
        eprintln!("skipping: no Metal adapter");
        return;
    };
    let (dev_b, queue_b, _) = metal_device().unwrap();
    eprintln!("adapter: {name}");

    for format in atd_interop::SHAREABLE_FORMATS {
        let shared = SharedSurface::create(&dev_a, W, H, format).unwrap();
        let pattern = test_pattern(W, H, 5);
        write_rgba8(&queue_a, &shared.texture, &pattern);
        finish(&dev_a, &queue_a);

        let surface = lookup(shared.id()).expect("surface is published");
        // SAFETY: the surface was created by `SharedSurface::create` with these parameters.
        let imported = unsafe { import(&dev_b, &surface, W, H, format) }.unwrap();
        assert_eq!(
            read_rgba8(&dev_b, &queue_b, &imported).unwrap(),
            pattern,
            "{format:?}: B reads A's pixels"
        );

        let reply = test_pattern(W, H, 77);
        write_rgba8(&queue_b, &imported, &reply);
        finish(&dev_b, &queue_b);
        assert_eq!(
            read_rgba8(&dev_a, &queue_a, &shared.texture).unwrap(),
            reply,
            "{format:?}: A reads B's pixels"
        );
    }
}
