//! Atidarla GPU layer (PRD §5.2): wgpu device and queue, texture and buffer
//! pools, per-frame render-graph compile, WGSL/WESL library.
//!
//! **Status:** device bring-up, adapter reporting and GPU-vendor detection
//! (M0); M1 pre-work: the texture pool ([`pool`]), full-screen passes
//! ([`fullscreen`]) and the cooking [`engine`]; M2 pre-work: geometry
//! values, the attribute buffer pool and compute building blocks ([`geo`]).

pub mod engine;
pub mod expr;
pub mod fullscreen;
pub mod geo;
pub mod isf;
pub mod mat;
pub mod pool;
pub mod timer;

use atd_core::{GpuVendor, PlatformInfo};
use serde::Serialize;

/// Error bringing up a GPU device.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),
    #[error("failed to create GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}

/// Serializable summary of the active adapter, for the UI and bench output.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterReport {
    pub name: String,
    pub vendor: GpuVendor,
    pub pci_vendor_id: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
    pub driver_info: String,
}

impl AdapterReport {
    pub fn from_info(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            vendor: vendor_of(info),
            pci_vendor_id: info.vendor,
            device_type: format!("{:?}", info.device_type),
            backend: format!("{:?}", info.backend),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
        }
    }
}

/// Classifies an adapter's vendor. Metal adapters do not always report a PCI
/// vendor ID, and CPU rasterisers are reported as [`GpuVendor::Software`]
/// whatever their ID, so they never satisfy a vendor capability flag.
pub fn vendor_of(info: &wgpu::AdapterInfo) -> GpuVendor {
    if info.device_type == wgpu::DeviceType::Cpu {
        return GpuVendor::Software;
    }
    match GpuVendor::from_pci_id(info.vendor) {
        GpuVendor::Unknown if info.backend == wgpu::Backend::Metal => GpuVendor::Apple,
        vendor => vendor,
    }
}

/// Startup platform detection: compile-time OS/arch plus the active adapter.
pub fn platform_info(info: &wgpu::AdapterInfo) -> PlatformInfo {
    PlatformInfo::host().with_gpu(
        vendor_of(info),
        info.name.clone(),
        format!("{:?}", info.backend),
    )
}

/// A device and queue not tied to any window, for the CLI, bench and tests.
#[derive(Debug)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// The instance settings for the app, the CLI and headless runs. `WGPU_BACKEND`
/// wins when set. Otherwise Windows uses **DX12 only** (PRD A24): Spout and
/// CUDA external memory share DX12 resources, and DX12 also keeps GL out of the
/// instance. Other platforms keep wgpu's defaults.
pub fn instance_descriptor() -> wgpu::InstanceDescriptor {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        desc.backends = wgpu::Backends::DX12;
    }
    desc
}

/// An instance for tests and tools that bring up many devices at once: the
/// settings of [`instance_descriptor`], and Vulkan, DX12 and Metal only
/// elsewhere. With GL enabled beside Vulkan, concurrent device creation
/// deadlocks on NVIDIA/Windows (M1 log, 2026-10-17), and Atidarla does not
/// target GL.
pub fn primary_instance() -> wgpu::Instance {
    let mut desc = instance_descriptor();
    if !cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        desc.backends = wgpu::Backends::PRIMARY;
    }
    wgpu::Instance::new(desc)
}

impl GpuContext {
    /// Brings up the highest-performance adapter without a surface. Honours
    /// the standard `WGPU_BACKEND` / `WGPU_ADAPTER_NAME` environment overrides.
    pub fn headless() -> Result<Self, GpuError> {
        pollster::block_on(Self::headless_async())
    }

    pub async fn headless_async() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(instance_descriptor());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::from_env()
                    .unwrap_or(wgpu::PowerPreference::HighPerformance),
                ..Default::default()
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("atd-headless"),
                required_features: optional_features(&adapter),
                required_limits: required_limits(&adapter),
                ..Default::default()
            })
            .await?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub fn report(&self) -> AdapterReport {
        AdapterReport::from_info(&self.adapter.get_info())
    }

    pub fn platform_info(&self) -> PlatformInfo {
        platform_info(&self.adapter.get_info())
    }
}

/// Device features Atidarla uses when the adapter has them, each with a
/// fallback when it does not (rule 6): BC texture compression (HAP movies
/// upload without CPU decoding).
pub fn optional_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC
}

/// Storage buffers per shader stage requested when the adapter offers them
/// (design: geometry-attributes.md §7; the WebGPU default is 8).
pub const TARGET_STORAGE_BUFFERS: u32 = 16;
/// Storage binding and buffer size requested when offered (1 GiB; the
/// WebGPU defaults are 128 MiB and 256 MiB).
pub const TARGET_BUFFER_BYTES: u64 = 1 << 30;
/// Largest 2D texture requested when offered.
pub const TARGET_TEXTURE_2D: u32 = 8192;

/// The device limits Atidarla asks for: the WebGPU defaults, raised to what
/// the adapter offers up to the targets above (storage buffers per stage,
/// storage binding and buffer size, 2D texture size). Every core operator
/// still works at the defaults; the raised limits only lift ceilings (more
/// points per buffer, wider kernels). GL adapters get the WebGL2 defaults.
pub fn required_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let offered = adapter.limits();
    let base = if adapter.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    raise_limits(base, &offered)
}

/// `base` with the GEO-relevant limits raised to `min(offered, target)`
/// (never below `base`).
pub fn raise_limits(base: wgpu::Limits, offered: &wgpu::Limits) -> wgpu::Limits {
    let up32 = |base: u32, offered: u32, target: u32| base.max(offered.min(target));
    let up64 = |base: u64, offered: u64, target: u64| base.max(offered.min(target));
    wgpu::Limits {
        max_storage_buffers_per_shader_stage: up32(
            base.max_storage_buffers_per_shader_stage,
            offered.max_storage_buffers_per_shader_stage,
            TARGET_STORAGE_BUFFERS,
        ),
        max_storage_buffer_binding_size: up64(
            base.max_storage_buffer_binding_size,
            offered.max_storage_buffer_binding_size,
            TARGET_BUFFER_BYTES,
        ),
        max_buffer_size: up64(
            base.max_buffer_size,
            offered.max_buffer_size,
            TARGET_BUFFER_BYTES,
        ),
        max_texture_dimension_2d: up32(
            base.max_texture_dimension_2d,
            offered.max_texture_dimension_2d,
            TARGET_TEXTURE_2D,
        ),
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        vendor: u32,
        device_type: wgpu::DeviceType,
        backend: wgpu::Backend,
    ) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: "test".into(),
            vendor,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend,
            ..wgpu::AdapterInfo::new(wgpu::DeviceType::Other, wgpu::Backend::Noop)
        }
    }

    #[test]
    fn limits_rise_to_what_is_offered_up_to_the_targets() {
        let base = wgpu::Limits::default();
        let mut offered = base.clone();
        offered.max_storage_buffers_per_shader_stage = 64;
        offered.max_storage_buffer_binding_size = 2 << 30;
        offered.max_buffer_size = 512 << 20;
        let got = raise_limits(base.clone(), &offered);
        assert_eq!(
            got.max_storage_buffers_per_shader_stage,
            TARGET_STORAGE_BUFFERS
        );
        assert_eq!(got.max_storage_buffer_binding_size, TARGET_BUFFER_BYTES);
        assert_eq!(got.max_buffer_size, 512 << 20, "what the adapter offers");
        // An adapter offering less than the defaults keeps the defaults
        // (requesting them then fails loudly, as before).
        let mut small = base.clone();
        small.max_storage_buffers_per_shader_stage = 4;
        let got = raise_limits(base.clone(), &small);
        assert_eq!(got.max_storage_buffers_per_shader_stage, 8);
        assert_eq!(
            got.max_bind_groups, base.max_bind_groups,
            "others unchanged"
        );
    }

    #[test]
    fn vendor_detection() {
        use wgpu::{Backend, DeviceType};
        assert_eq!(
            vendor_of(&info(0x10DE, DeviceType::DiscreteGpu, Backend::Dx12)),
            GpuVendor::Nvidia
        );
        assert_eq!(
            vendor_of(&info(0, DeviceType::IntegratedGpu, Backend::Metal)),
            GpuVendor::Apple
        );
        assert_eq!(
            vendor_of(&info(0x10DE, DeviceType::Cpu, Backend::Vulkan)),
            GpuVendor::Software
        );
        assert_eq!(
            vendor_of(&info(0, DeviceType::Other, Backend::Vulkan)),
            GpuVendor::Unknown
        );
    }
}
