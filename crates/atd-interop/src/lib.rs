//! Per-OS GPU texture sharing (PRD §5.2 `atd-interop`, M0 spikes a and b).
//!
//! - **Windows (DX12):** textures backed by a `D3D12_HEAP_FLAG_SHARED`
//!   resource, exported as NT handles. The same handle feeds D3D11 and
//!   CUDA external memory (`cudaImportExternalMemory` with
//!   `cudaExternalMemoryHandleTypeD3D12Resource`), see [`dx12`].
//!   [`spout`] speaks the Spout 2.007 protocol on top of it, with one GPU
//!   copy into or out of Spout's legacy-shared D3D11 texture.
//! - **macOS (Metal):** textures backed by an `IOSurface`, shareable by
//!   `IOSurfaceID` or mach port; the basis of Syphon, see [`metal`].
//! - **Everywhere:** [`readback`] copies a texture to CPU memory, the
//!   fallback when no zero-copy path exists (RESEARCH §4.8).
//!
//! Everything here runs outside the render thread's hot path or once at
//! set-up; per-frame sharing is a handle hand-off or a GPU copy, never a
//! CPU copy (except [`readback`]).

pub mod readback;

#[cfg(windows)]
pub mod dx12;

#[cfg(windows)]
pub mod spout;

#[cfg(target_os = "macos")]
pub mod metal;

/// Errors from the interop layer.
#[derive(Debug, thiserror::Error)]
pub enum InteropError {
    #[error("the wgpu device is not running on the {0} backend")]
    WrongBackend(&'static str),
    #[error("unsupported texture format {0:?} for sharing")]
    UnsupportedFormat(wgpu::TextureFormat),
    #[error("platform call failed: {0}")]
    Platform(String),
    #[error("GPU error: {0}")]
    Gpu(String),
}

/// Formats every sharing path supports: 8-bit RGBA/BGRA, the formats Spout,
/// Syphon and CUDA interop agree on.
pub const SHAREABLE_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Rgba8Unorm,
    wgpu::TextureFormat::Bgra8Unorm,
];

/// Usages a shared texture is created with.
pub const SHARED_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

#[cfg(any(windows, target_os = "macos"))]
fn check_format(format: wgpu::TextureFormat) -> Result<(), InteropError> {
    if SHAREABLE_FORMATS.contains(&format) {
        Ok(())
    } else {
        Err(InteropError::UnsupportedFormat(format))
    }
}

/// Descriptor shared by the export and import paths.
#[cfg(any(windows, target_os = "macos"))]
fn texture_descriptor(
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("atd-shared"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: SHARED_USAGE,
        view_formats: &[],
    }
}
