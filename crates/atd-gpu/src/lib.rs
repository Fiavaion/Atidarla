//! Atidarla GPU layer (PRD §5.2): wgpu device and queue, texture and buffer
//! pools, per-frame render-graph compile, WGSL/WESL library.
//!
//! **Status (M0):** device bring-up, adapter reporting and GPU-vendor
//! detection for capability flags. Pools and the render graph land in M1.

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

impl GpuContext {
    /// Brings up the highest-performance adapter without a surface. Honours
    /// the standard `WGPU_BACKEND` / `WGPU_ADAPTER_NAME` environment overrides.
    pub fn headless() -> Result<Self, GpuError> {
        pollster::block_on(Self::headless_async())
    }

    pub async fn headless_async() -> Result<Self, GpuError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
