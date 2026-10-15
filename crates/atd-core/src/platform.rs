//! Platform and GPU-vendor detection.
//!
//! The OS and architecture are known at compile time. The GPU vendor comes
//! from the wgpu adapter at startup (see `atd-gpu`), and optional SDKs
//! (e.g. a Kinect runtime) are probed by the crates that wrap them and added
//! with [`PlatformInfo::with_sdk`].

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Operating system. Windows and macOS are supported targets; Linux is
/// best-effort (PRD A18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    MacOs,
    Linux,
    Other,
}

impl Os {
    /// The OS this binary was compiled for.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        }
    }

    /// Short token used in capability flags (`win`, `mac`, `linux`).
    pub const fn token(self) -> &'static str {
        match self {
            Os::Windows => "win",
            Os::MacOs => "mac",
            Os::Linux => "linux",
            Os::Other => "other",
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Os::Windows => "Windows",
            Os::MacOs => "macOS",
            Os::Linux => "Linux",
            Os::Other => "other",
        })
    }
}

/// CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    Aarch64,
    Other,
}

impl Arch {
    /// The architecture this binary was compiled for.
    pub const fn current() -> Self {
        if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else {
            Arch::Other
        }
    }
}

/// GPU vendor of the active adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    /// A CPU/software rasteriser (e.g. lavapipe, WARP, llvmpipe).
    Software,
    Unknown,
}

impl GpuVendor {
    /// Maps a PCI vendor ID (as reported by wgpu's `AdapterInfo::vendor`).
    pub const fn from_pci_id(id: u32) -> Self {
        match id {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            0x106B => GpuVendor::Apple,
            // Mesa's software rasterisers report 0x10005 (VK_VENDOR_ID_MESA).
            0x10005 => GpuVendor::Software,
            _ => GpuVendor::Unknown,
        }
    }

    /// Short token used in capability flags.
    pub const fn token(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
            GpuVendor::Intel => "intel",
            GpuVendor::Apple => "apple",
            GpuVendor::Software => "software",
            GpuVendor::Unknown => "unknown",
        }
    }
}

impl fmt::Display for GpuVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GpuVendor::Nvidia => "NVIDIA",
            GpuVendor::Amd => "AMD",
            GpuVendor::Intel => "Intel",
            GpuVendor::Apple => "Apple",
            GpuVendor::Software => "Software",
            GpuVendor::Unknown => "Unknown",
        })
    }
}

/// Everything capability flags are evaluated against, detected once at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub os: Os,
    pub arch: Arch,
    pub gpu_vendor: GpuVendor,
    /// Adapter name as reported by the driver, if a GPU was initialised.
    pub gpu_name: Option<String>,
    /// Graphics backend in use (`Vulkan`, `Dx12`, `Metal`, ...), if any.
    pub gpu_backend: Option<String>,
    /// Optional runtimes/SDKs found on this machine (e.g. `kinect`, `ndi`).
    pub sdks: BTreeSet<String>,
}

impl PlatformInfo {
    /// Compile-time facts only; no GPU yet.
    pub fn host() -> Self {
        Self {
            os: Os::current(),
            arch: Arch::current(),
            gpu_vendor: GpuVendor::Unknown,
            gpu_name: None,
            gpu_backend: None,
            sdks: BTreeSet::new(),
        }
    }

    /// Records the active GPU adapter.
    pub fn with_gpu(
        mut self,
        vendor: GpuVendor,
        name: impl Into<String>,
        backend: impl Into<String>,
    ) -> Self {
        self.gpu_vendor = vendor;
        self.gpu_name = Some(name.into());
        self.gpu_backend = Some(backend.into());
        self
    }

    /// Records an optional SDK/runtime as present.
    pub fn with_sdk(mut self, sdk: impl Into<String>) -> Self {
        self.sdks.insert(sdk.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_vendor_ids_map() {
        assert_eq!(GpuVendor::from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(GpuVendor::from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(GpuVendor::from_pci_id(0x106B), GpuVendor::Apple);
        assert_eq!(GpuVendor::from_pci_id(0x10005), GpuVendor::Software);
        assert_eq!(GpuVendor::from_pci_id(0xDEAD), GpuVendor::Unknown);
    }

    #[test]
    fn host_reports_compile_target() {
        let host = PlatformInfo::host();
        assert_eq!(host.os, Os::current());
        assert_eq!(host.gpu_vendor, GpuVendor::Unknown);
        assert!(host.gpu_name.is_none());
    }
}
