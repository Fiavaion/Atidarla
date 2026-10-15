//! Round trip through a GPU texture on whatever adapter is available.

use atd_interop::readback::{read_rgba8, test_pattern, write_rgba8};

/// Skips without a GPU, unless `ATD_REQUIRE_GPU` is set (CI on Windows and
/// macOS), where a missing adapter is a failure.
fn no_gpu(what: &str) {
    let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "ATD_REQUIRE_GPU is set but there is no {what}");
    eprintln!("skipping: no {what}");
}

#[test]
fn write_then_read_back() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        no_gpu("GPU adapter");
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    // 70 px wide forces row padding (280 → 512 bytes).
    let (w, h) = (70, 9);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let pattern = test_pattern(w, h, 3);
    write_rgba8(&queue, &texture, &pattern);
    assert_eq!(read_rgba8(&device, &queue, &texture).unwrap(), pattern);
}
