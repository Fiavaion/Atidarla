//! Spike (a)/(b) on Windows: a texture written on one D3D12 device is read
//! on an independent device through the shared NT handle, without a CPU copy.
#![cfg(windows)]

use lsr_interop::dx12::{SharedTexture, import, open_by_name};
use lsr_interop::readback::{read_rgba8, test_pattern, write_rgba8};

/// Skips without a GPU, unless `LSR_REQUIRE_GPU` is set (CI on Windows and
/// macOS), where a missing adapter is a failure.
fn no_gpu(what: &str) {
    let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "LSR_REQUIRE_GPU is set but there is no {what}");
    eprintln!("skipping: no {what}");
}

const W: u32 = 96;
const H: u32 = 40;

/// A device on its own DX12 instance, like a second process would have.
fn dx12_device() -> Option<(wgpu::Device, wgpu::Queue, String)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let name = format!(
        "{} ({:?})",
        adapter.get_info().name,
        adapter.get_info().device_type
    );
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
fn shared_texture_crosses_devices() {
    let Some((dev_a, queue_a, name)) = dx12_device() else {
        no_gpu("DX12 adapter");
        return;
    };
    let (dev_b, queue_b, _) = dx12_device().unwrap();
    eprintln!("adapter: {name}");

    for format in lsr_interop::SHAREABLE_FORMATS {
        let shared = SharedTexture::create(&dev_a, W, H, format, None).unwrap();
        let pattern = test_pattern(W, H, 11);
        write_rgba8(&queue_a, &shared.texture, &pattern);
        finish(&dev_a, &queue_a);

        // SAFETY: the handle comes from `SharedTexture::create` with these parameters.
        let imported = unsafe { import(&dev_b, shared.handle.raw(), W, H, format) }.unwrap();
        assert_eq!(
            read_rgba8(&dev_b, &queue_b, &imported).unwrap(),
            pattern,
            "{format:?}: B reads A's pixels"
        );

        // And back: B writes, A reads.
        let reply = test_pattern(W, H, 99);
        write_rgba8(&queue_b, &imported, &reply);
        finish(&dev_b, &queue_b);
        assert_eq!(
            read_rgba8(&dev_a, &queue_a, &shared.texture).unwrap(),
            reply,
            "{format:?}: A reads B's pixels"
        );
    }
}

#[test]
fn shared_texture_opens_by_name() {
    let Some((dev_a, queue_a, _)) = dx12_device() else {
        no_gpu("DX12 adapter");
        return;
    };
    let (dev_b, queue_b, _) = dx12_device().unwrap();
    let name = format!("Local\\lasair-test-{}", std::process::id());
    let fmt = wgpu::TextureFormat::Bgra8Unorm;
    let shared = SharedTexture::create(&dev_a, W, H, fmt, Some(&name)).unwrap();
    let pattern = test_pattern(W, H, 42);
    write_rgba8(&queue_a, &shared.texture, &pattern);
    finish(&dev_a, &queue_a);

    let handle = open_by_name(&dev_b, &name).unwrap();
    // SAFETY: the named handle refers to the texture created above.
    let imported = unsafe { import(&dev_b, handle.raw(), W, H, fmt) }.unwrap();
    assert_eq!(read_rgba8(&dev_b, &queue_b, &imported).unwrap(), pattern);
}

#[test]
fn rejects_unsupported_format() {
    let Some((dev, _, _)) = dx12_device() else {
        return;
    };
    let err =
        SharedTexture::create(&dev, W, H, wgpu::TextureFormat::Rgba16Float, None).unwrap_err();
    assert!(matches!(
        err,
        lsr_interop::InteropError::UnsupportedFormat(_)
    ));
}
