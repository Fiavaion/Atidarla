//! The D3D11 side of Spout: a device on the wgpu device's adapter, legacy
//! shared textures, and a shared fence that orders D3D11 copies against
//! the wgpu (D3D12) queue on the GPU without blocking the CPU.

use std::ffi::c_void;

use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{GENERIC_ALL, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11CreateDevice, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext4, ID3D11Fence,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{D3D12_FENCE_FLAG_SHARED, ID3D12Fence};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4, IDXGIResource,
};
use windows::core::{Interface, PCWSTR};

use crate::InteropError;
use crate::dx12::{SharedHandle, platform, with_raw_device};

fn missing(what: &str) -> InteropError {
    InteropError::Platform(format!("D3D11 returned no {what}"))
}

/// The wgpu format a Spout sender's texture can be copied into: same
/// typeless group, so `CopyResource` accepts it.
pub(super) fn wgpu_format(format: DXGI_FORMAT) -> Option<wgpu::TextureFormat> {
    match format {
        DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => {
            Some(wgpu::TextureFormat::Bgra8Unorm)
        }
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => {
            Some(wgpu::TextureFormat::Rgba8Unorm)
        }
        _ => None,
    }
}

/// A D3D11 device on the same adapter as a wgpu DX12 device, with a fence
/// shared between the two.
#[derive(Debug)]
pub(super) struct D3D11 {
    device: ID3D11Device1,
    context: ID3D11DeviceContext4,
    fence: ID3D11Fence,
    /// The same fence as seen by D3D12, for the wgpu queue.
    fence12: ID3D12Fence,
    value: u64,
}

impl D3D11 {
    /// Creates the device on the adapter whose LUID matches `device`'s.
    pub(super) fn new(device: &wgpu::Device) -> Result<Self, InteropError> {
        let (luid, fence12, fence_handle) = with_raw_device(device, |raw| {
            // SAFETY: plain queries and object creation on a live device.
            unsafe {
                let luid = raw.GetAdapterLuid();
                let fence12: ID3D12Fence = raw
                    .CreateFence(0, D3D12_FENCE_FLAG_SHARED)
                    .map_err(platform)?;
                let handle = raw
                    .CreateSharedHandle(&fence12, None, GENERIC_ALL.0, PCWSTR::null())
                    .map_err(platform)?;
                Ok((luid, fence12, SharedHandle(handle)))
            }
        })?;

        // SAFETY: DXGI factory and adapter lookup have no preconditions.
        let adapter: IDXGIAdapter = unsafe {
            CreateDXGIFactory1::<IDXGIFactory4>()
                .and_then(|f| f.EnumAdapterByLuid(luid))
                .map_err(platform)?
        };
        let (mut device, mut context) = (None, None);
        // SAFETY: out-pointers are locals; the adapter is valid.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(platform)?;
        let device: ID3D11Device1 = device
            .ok_or_else(|| missing("device"))?
            .cast()
            .map_err(platform)?;
        let context: ID3D11DeviceContext4 = context
            .ok_or_else(|| missing("context"))?
            .cast()
            .map_err(platform)?;

        // D3D11.4 shared fences need Windows 10 1703 or later.
        let device5: ID3D11Device5 = device.cast().map_err(platform)?;
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: the handle is a shared fence handle created above.
        unsafe { device5.OpenSharedFence(fence_handle.raw(), &mut fence) }.map_err(platform)?;
        Ok(Self {
            device,
            context,
            fence: fence.ok_or_else(|| missing("fence"))?,
            fence12,
            value: 0,
        })
    }

    /// Creates a texture Spout receivers can open: render target and shader
    /// resource, one mip, one sample, `MISC_SHARED` (legacy handle). Returns
    /// it with its handle truncated to 32 bits, as Spout stores it.
    pub(super) fn create_shared(
        &self,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
    ) -> Result<(ID3D11Texture2D, u32), InteropError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };
        let mut texture = None;
        // SAFETY: valid descriptor; the out-pointer is a local Option.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(platform)?;
        let texture: ID3D11Texture2D = texture.ok_or_else(|| missing("texture"))?;
        let resource: IDXGIResource = texture.cast().map_err(platform)?;
        // SAFETY: the texture was created with MISC_SHARED.
        let handle = unsafe { resource.GetSharedHandle() }.map_err(platform)?;
        // Legacy shared handles fit in 32 bits on 64-bit Windows too
        // (the SDK's `PtrToUint`).
        Ok((texture, handle.0 as usize as u32))
    }

    /// Opens a Spout sender's texture from its 32-bit legacy handle,
    /// sign-extended back as the SDK's `LongToHandle` does.
    pub(super) fn open_legacy(
        &self,
        handle: u32,
    ) -> Result<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC), InteropError> {
        let handle = HANDLE(handle as i32 as isize as *mut c_void);
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: an invalid or foreign handle makes the call fail, it is
        // not dereferenced by us.
        unsafe { self.device.OpenSharedResource(handle, &mut texture) }.map_err(platform)?;
        let texture = texture.ok_or_else(|| missing("texture"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a local out-parameter.
        unsafe { texture.GetDesc(&mut desc) };
        Ok((texture, desc))
    }

    /// Opens a [`crate::dx12::SharedTexture`]'s NT handle on this device.
    pub(super) fn open_nt(&self, handle: HANDLE) -> Result<ID3D11Texture2D, InteropError> {
        // SAFETY: the handle is owned by a live `SharedTexture`.
        unsafe { self.device.OpenSharedResource1(handle) }.map_err(platform)
    }

    /// Copies `src` into `dst` after all work submitted to `queue` so far,
    /// and before any work submitted to it later. The queue signals the
    /// shared fence, D3D11 waits for it, copies, signals again and flushes,
    /// and the queue waits for that. Both waits are GPU-side.
    pub(super) fn copy(
        &mut self,
        queue: &wgpu::Queue,
        dst: &ID3D11Texture2D,
        src: &ID3D11Texture2D,
    ) -> Result<(), InteropError> {
        // SAFETY: only `Signal` and `Wait` are called on the raw queue; both
        // are free-threaded and leave wgpu's own state alone.
        let hal = unsafe { queue.as_hal::<Dx12>() }.ok_or(InteropError::WrongBackend("DX12"))?;
        let raw = hal.as_raw();
        let ready = self.value + 1;
        let done = self.value + 2;
        self.value = done;
        // SAFETY: live COM objects on one adapter; `dst` and `src` have the
        // same size and compatible formats (checked by the callers).
        unsafe {
            raw.Signal(&self.fence12, ready).map_err(platform)?;
            self.context.Wait(&self.fence, ready).map_err(platform)?;
            self.context.CopyResource(dst, src);
            self.context.Signal(&self.fence, done).map_err(platform)?;
            self.context.Flush();
            raw.Wait(&self.fence12, done).map_err(platform)?;
        }
        Ok(())
    }

    /// Submits pending D3D11 work, which also lets released textures go.
    pub(super) fn flush(&self) {
        // SAFETY: the context is live and used from one thread at a time.
        unsafe { self.context.Flush() };
    }
}
