//! Texture pool: frame-critical code reuses textures instead of creating
//! them (CLAUDE.md rule 2). Textures are bucketed by size, format and usage.
//!
//! Free textures that sit unused are dropped: per-node output sizes mean a
//! dragged Resolution or Crop slider visits a new size every frame, and
//! without trimming each size would stay in video memory for the session.
//! A texture is dropped once it has been free for [`KEEP_FRAMES`] frames,
//! and the oldest go first whenever free textures exceed [`FREE_BUDGET`].
//! Steady state is unaffected: a node that cooks every frame takes its
//! texture back one frame after releasing it.

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

    /// Video memory of one such texture, in bytes (an estimate for formats
    /// without a fixed block size).
    pub fn bytes(&self) -> u64 {
        let (bw, bh) = self.format.block_dimensions();
        let block = u64::from(self.format.block_copy_size(None).unwrap_or(4));
        u64::from(self.width.div_ceil(bw)) * u64::from(self.height.div_ceil(bh)) * block
    }
}

/// Frames a free texture is kept before it is dropped (5 s at 60 fps).
pub const KEEP_FRAMES: u64 = 300;

/// Most video memory kept in free textures (bytes).
pub const FREE_BUDGET: u64 = 512 << 20;

#[derive(Debug)]
pub struct TexturePool {
    device: wgpu::Device,
    /// Free textures and the frame each was released in.
    free: HashMap<TexKey, Vec<(wgpu::Texture, u64)>>,
    free_bytes: u64,
    frame: u64,
    created: usize,
}

impl TexturePool {
    pub fn new(device: wgpu::Device) -> Self {
        Self {
            device,
            free: HashMap::new(),
            free_bytes: 0,
            frame: 0,
            created: 0,
        }
    }

    /// A texture matching `key`, reused when one is free.
    pub fn acquire(&mut self, key: TexKey) -> wgpu::Texture {
        if let Some((t, _)) = self.free.get_mut(&key).and_then(Vec::pop) {
            self.free_bytes -= key.bytes();
            return t;
        }
        self.created += 1;
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lsr-pool"),
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
        let key = TexKey::of(&texture);
        self.free_bytes += key.bytes();
        self.free
            .entry(key)
            .or_default()
            .push((texture, self.frame));
    }

    /// Ends a frame: drops textures free for longer than [`KEEP_FRAMES`],
    /// then the oldest free ones while they exceed [`FREE_BUDGET`] (never
    /// those released this frame, which the next frame reuses). Allocates
    /// nothing.
    pub fn end_frame(&mut self) {
        let now = self.frame;
        let mut dropped = 0;
        for (key, list) in &mut self.free {
            let before = list.len();
            list.retain(|(_, at)| now - at < KEEP_FRAMES);
            dropped += (before - list.len()) as u64 * key.bytes();
        }
        self.free_bytes -= dropped;
        let mut trimmed = dropped > 0;
        while self.free_bytes > FREE_BUDGET {
            trimmed = true;
            let oldest = self
                .free
                .iter()
                .filter_map(|(k, list)| list.first().map(|(_, at)| (*at, *k)))
                // Textures released this frame are what next frame reuses.
                .filter(|(at, _)| *at < now)
                .min_by_key(|(at, _)| *at);
            let Some((_, key)) = oldest else { break };
            if let Some(list) = self.free.get_mut(&key) {
                list.remove(0);
                self.free_bytes -= key.bytes();
            }
        }
        // Empty buckets of trimmed sizes go too. Only after a trim: in steady
        // state a bucket can be momentarily empty, and recreating it would
        // allocate.
        if trimmed {
            self.free.retain(|_, list| !list.is_empty());
        }
        self.frame += 1;
    }

    /// Total textures ever created (a steady-state frame should not raise it).
    pub fn created(&self) -> usize {
        self.created
    }

    /// Textures currently free for reuse.
    pub fn free_count(&self) -> usize {
        self.free.values().map(Vec::len).sum()
    }

    /// Video memory held by free textures, in bytes.
    pub fn free_bytes(&self) -> u64 {
        self.free_bytes
    }
}
