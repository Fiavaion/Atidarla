//! Texture pool: frame-critical code reuses textures instead of creating
//! them (CLAUDE.md rule 2). Textures are bucketed by size, format and usage.

use std::collections::HashMap;

/// What makes two textures interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TexKey {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

impl TexKey {
    pub fn of(texture: &wgpu::Texture) -> Self {
        Self {
            width: texture.width(),
            height: texture.height(),
            format: texture.format(),
            usage: texture.usage(),
        }
    }
}

#[derive(Debug)]
pub struct TexturePool {
    device: wgpu::Device,
    free: HashMap<TexKey, Vec<wgpu::Texture>>,
    created: usize,
}

impl TexturePool {
    pub fn new(device: wgpu::Device) -> Self {
        Self {
            device,
            free: HashMap::new(),
            created: 0,
        }
    }

    /// A texture matching `key`, reused when one is free.
    pub fn acquire(&mut self, key: TexKey) -> wgpu::Texture {
        if let Some(t) = self.free.get_mut(&key).and_then(Vec::pop) {
            return t;
        }
        self.created += 1;
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atd-pool"),
            size: wgpu::Extent3d {
                width: key.width,
                height: key.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: key.format,
            usage: key.usage,
            view_formats: &[],
        })
    }

    /// Returns a texture for reuse. The caller must not use it afterwards and
    /// must only release it once the GPU work reading it has been submitted.
    pub fn release(&mut self, texture: wgpu::Texture) {
        self.free
            .entry(TexKey::of(&texture))
            .or_default()
            .push(texture);
    }

    /// Total textures ever created (a steady-state frame should not raise it).
    pub fn created(&self) -> usize {
        self.created
    }

    /// Textures currently free for reuse.
    pub fn free_count(&self) -> usize {
        self.free.values().map(Vec::len).sum()
    }
}
