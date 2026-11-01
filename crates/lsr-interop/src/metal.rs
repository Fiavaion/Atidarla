//! macOS: wgpu textures backed by IOSurfaces (spike a, Syphon side).
//!
//! [`SharedSurface::create`] allocates an `IOSurface`, creates a Metal
//! texture on it and wraps that as a `wgpu::Texture`. Another device or
//! process finds the surface by [`SharedSurface::id`] (or a mach port) and
//! wraps it with [`import`]. Syphon servers publish exactly such surfaces.
//! Producer/consumer ordering is the caller's job (`Device::poll` in tests;
//! a shared event or the Syphon frame protocol in production).

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_io_surface::{
    IOSurfaceID, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat,
    kIOSurfaceWidth,
};
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage,
};
use wgpu::hal::api::Metal;

use crate::{InteropError, check_format, texture_descriptor};

/// FourCC pixel formats (`'BGRA'`, `'RGBA'`) for IOSurface.
const fn four_cc(code: &[u8; 4]) -> i32 {
    i32::from_be_bytes(*code)
}

fn formats(format: wgpu::TextureFormat) -> Result<(MTLPixelFormat, i32), InteropError> {
    check_format(format)?;
    Ok(match format {
        wgpu::TextureFormat::Bgra8Unorm => (MTLPixelFormat::BGRA8Unorm, four_cc(b"BGRA")),
        _ => (MTLPixelFormat::RGBA8Unorm, four_cc(b"RGBA")),
    })
}

/// Creates a Metal texture on `surface` using the raw device behind `device`
/// and wraps it as a wgpu texture.
///
/// # Safety
/// `surface` must be at least `width` × `height` with 4 bytes per element in
/// the pixel layout matching `format`.
unsafe fn wrap(
    device: &wgpu::Device,
    surface: &IOSurfaceRef,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Result<wgpu::Texture, InteropError> {
    let (mtl_format, _) = formats(format)?;
    // SAFETY: the raw device is only used to create a texture.
    let hal = unsafe { device.as_hal::<Metal>() }.ok_or(InteropError::WrongBackend("Metal"))?;
    let raw: Retained<ProtocolObject<dyn MTLTexture>> = {
        // SAFETY: valid pixel format and non-zero size.
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format,
                width as usize,
                height as usize,
                false,
            )
        };
        desc.setUsage(
            MTLTextureUsage::ShaderRead
                | MTLTextureUsage::ShaderWrite
                | MTLTextureUsage::RenderTarget,
        );
        hal.raw_device()
            .newTextureWithDescriptor_iosurface_plane(&desc, surface, 0)
            .ok_or_else(|| {
                InteropError::Platform(
                    "newTextureWithDescriptor:iosurface:plane: returned nil".into(),
                )
            })?
    };
    drop(hal);

    let desc = texture_descriptor(width, height, format);
    // SAFETY: `raw` is a 2D, single-mip texture of exactly this size and format.
    let texture = unsafe {
        let hal_texture = wgpu::hal::metal::Device::texture_from_raw(
            raw,
            format,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent {
                width,
                height,
                depth: 1,
            },
            None,
        );
        device.create_texture_from_hal::<Metal>(
            hal_texture,
            &desc,
            wgpu::TextureUses::UNINITIALIZED,
        )
    };
    Ok(texture)
}

/// A wgpu texture whose memory is an IOSurface other processes can open.
#[derive(Debug)]
pub struct SharedSurface {
    pub texture: wgpu::Texture,
    surface: CFRetained<IOSurfaceRef>,
}

impl SharedSurface {
    pub fn create(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self, InteropError> {
        let (_, pixel_format) = formats(format)?;
        let w = CFNumber::new_i32(width as i32);
        let h = CFNumber::new_i32(height as i32);
        let bpe = CFNumber::new_i32(4);
        let pf = CFNumber::new_i32(pixel_format);
        // SAFETY: the keys are immutable CFString constants exported by IOSurface.
        let keys: [&CFString; 4] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ]
        };
        let props =
            CFDictionary::<CFString, CFNumber>::from_slices(&keys, &[&*w, &*h, &*bpe, &*pf]);
        // SAFETY: the dictionary holds the documented key/value types.
        let surface = unsafe { IOSurfaceRef::new(props.as_ref()) }
            .ok_or_else(|| InteropError::Platform("IOSurfaceCreate failed".into()))?;
        // SAFETY: the surface was just created with this size and layout.
        let texture = unsafe { wrap(device, &surface, width, height, format) }?;
        Ok(Self { texture, surface })
    }

    /// Process-global ID other processes can pass to [`lookup`].
    pub fn id(&self) -> IOSurfaceID {
        self.surface.id()
    }

    pub fn surface(&self) -> &IOSurfaceRef {
        &self.surface
    }
}

/// Finds a surface published by any process by its ID.
pub fn lookup(id: IOSurfaceID) -> Option<CFRetained<IOSurfaceRef>> {
    IOSurfaceRef::lookup(id)
}

/// Wraps an existing IOSurface as a wgpu texture on a Metal `device`.
///
/// # Safety
/// `surface` must be at least `width` × `height`, 4 bytes per element, in
/// the layout matching `format`.
pub unsafe fn import(
    device: &wgpu::Device,
    surface: &IOSurfaceRef,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Result<wgpu::Texture, InteropError> {
    // SAFETY: guaranteed by the caller.
    unsafe { wrap(device, surface, width, height, format) }
}
