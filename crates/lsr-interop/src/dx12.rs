//! Windows: wgpu textures backed by shared D3D12 resources (spikes a and b).
//!
//! [`SharedTexture::create`] allocates a committed resource on a
//! `D3D12_HEAP_FLAG_SHARED` heap, exports it as an NT handle and wraps it as
//! a `wgpu::Texture`. [`import`] opens such a handle on another device, in
//! this or another process, and wraps it the same way. The resource uses
//! `D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS`, so it decays to the
//! common state between submissions and needs no cross-device barriers;
//! ordering between producer and consumer is the caller's job (a shared
//! fence in production, `Device::poll` in tests).
//!
//! The NT handle is what D3D11 (`OpenSharedResource1`, used by
//! [`crate::spout`]) and CUDA (`cudaImportExternalMemory`, type
//! `D3D12Resource`) consume. Spout itself shares legacy D3D11 handles.

use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
    D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS, D3D12_RESOURCE_STATE_COMMON,
    D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Device, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::core::PCWSTR;

use crate::{InteropError, check_format, texture_descriptor};

pub(crate) fn platform(e: windows::core::Error) -> InteropError {
    InteropError::Platform(e.to_string())
}

pub(crate) fn dxgi_format(format: wgpu::TextureFormat) -> Result<DXGI_FORMAT, InteropError> {
    check_format(format)?;
    Ok(match format {
        wgpu::TextureFormat::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        _ => DXGI_FORMAT_R8G8B8A8_UNORM,
    })
}

/// Runs `f` with the raw D3D12 device behind a wgpu device.
pub(crate) fn with_raw_device<R>(
    device: &wgpu::Device,
    f: impl FnOnce(&ID3D12Device) -> Result<R, InteropError>,
) -> Result<R, InteropError> {
    // SAFETY: the raw device is only used for resource creation and handle
    // calls, which are thread-safe in D3D12 and do not affect wgpu's state.
    let hal = unsafe { device.as_hal::<Dx12>() }.ok_or(InteropError::WrongBackend("DX12"))?;
    f(hal.raw_device())
}

/// Wraps a D3D12 resource as a wgpu texture on `device`.
///
/// # Safety
/// `resource` must be a 2D, single-mip, single-sample texture of the given
/// size and format, created with render-target and simultaneous access flags,
/// on the same adapter as `device`.
unsafe fn wrap(
    device: &wgpu::Device,
    resource: ID3D12Resource,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    let desc = texture_descriptor(width, height, format);
    // SAFETY: guaranteed by the caller; the resource is in the common state.
    unsafe {
        let hal = wgpu::hal::dx12::Device::texture_from_raw(
            resource,
            format,
            wgpu::TextureDimension::D2,
            desc.size,
            1,
            1,
        );
        device.create_texture_from_hal::<Dx12>(hal, &desc, wgpu::TextureUses::UNINITIALIZED)
    }
}

/// An owned NT handle to a shared resource. Closed on drop.
#[derive(Debug)]
pub struct SharedHandle(pub(crate) HANDLE);

// SAFETY: an NT handle is a process-wide value, usable from any thread.
unsafe impl Send for SharedHandle {}
unsafe impl Sync for SharedHandle {}

impl SharedHandle {
    /// The raw handle value, e.g. to duplicate into another process.
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for SharedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A wgpu texture whose memory can be opened by other devices and processes.
#[derive(Debug)]
pub struct SharedTexture {
    pub texture: wgpu::Texture,
    pub handle: SharedHandle,
}

impl SharedTexture {
    /// Creates a shareable texture on a DX12 `device`. With `name`, the handle
    /// is also published under that name for `OpenSharedHandleByName`.
    pub fn create(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        name: Option<&str>,
    ) -> Result<Self, InteropError> {
        let dxgi = dxgi_format(format)?;
        let (resource, handle) = with_raw_device(device, |raw| {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Alignment: 0,
                Width: u64::from(width),
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: dxgi,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                    | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
            };
            let mut resource: Option<ID3D12Resource> = None;
            // SAFETY: valid descriptors; the out-pointer is a local Option.
            unsafe {
                raw.CreateCommittedResource(
                    &heap,
                    D3D12_HEAP_FLAG_SHARED,
                    &desc,
                    D3D12_RESOURCE_STATE_COMMON,
                    None,
                    &mut resource,
                )
            }
            .map_err(platform)?;
            let resource =
                resource.ok_or_else(|| InteropError::Platform("no resource returned".into()))?;

            let wide: Option<Vec<u16>> = name.map(|n| n.encode_utf16().chain([0]).collect());
            let pname = wide.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
            // SAFETY: `pname` is null or a NUL-terminated UTF-16 string that
            // outlives the call.
            let handle = unsafe { raw.CreateSharedHandle(&resource, None, GENERIC_ALL.0, pname) }
                .map_err(platform)?;
            Ok((resource, SharedHandle(handle)))
        })?;
        // SAFETY: the resource was created above with matching parameters.
        let texture = unsafe { wrap(device, resource, width, height, format) };
        Ok(Self { texture, handle })
    }
}

/// Opens a shared NT handle on a DX12 `device` as a wgpu texture.
///
/// # Safety
/// `handle` must refer to a resource created by [`SharedTexture::create`]
/// (or an equivalent producer) with the given size and format, on the same
/// adapter, and must stay valid for this call.
pub unsafe fn import(
    device: &wgpu::Device,
    handle: HANDLE,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Result<wgpu::Texture, InteropError> {
    check_format(format)?;
    let resource = with_raw_device(device, |raw| {
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: the caller guarantees the handle is valid.
        unsafe { raw.OpenSharedHandle(handle, &mut resource) }.map_err(platform)?;
        resource.ok_or_else(|| InteropError::Platform("no resource returned".into()))
    })?;
    // SAFETY: guaranteed by the caller.
    Ok(unsafe { wrap(device, resource, width, height, format) })
}

/// Opens a handle published under `name` by [`SharedTexture::create`].
/// The returned handle is owned and closed on drop.
pub fn open_by_name(device: &wgpu::Device, name: &str) -> Result<SharedHandle, InteropError> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    with_raw_device(device, |raw| {
        // SAFETY: NUL-terminated UTF-16 string valid for the call.
        unsafe { raw.OpenSharedHandleByName(PCWSTR(wide.as_ptr()), GENERIC_ALL.0) }
            .map(SharedHandle)
            .map_err(platform)
    })
}
