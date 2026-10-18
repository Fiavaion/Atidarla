//! Buffer pool for geometry attributes (design D5): frame-critical code
//! reuses storage buffers instead of creating them (CLAUDE.md rule 2).
//!
//! Buffers are bucketed by size class (powers of two from 4 KiB, so a dragged
//! count parameter reuses a buffer instead of creating one per value) and
//! usage. A [`GeoBuffer`] returns its buffer to the pool when dropped, from
//! any thread; returned buffers become reusable only at [`BufferPool::end_frame`],
//! which the engine calls after `queue.submit`, because a submitted frame may
//! still read them. Trimming follows the texture pool: a free buffer is
//! dropped after [`KEEP_FRAMES`] frames, and the oldest go first while free
//! memory exceeds [`FREE_BUDGET`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

pub use crate::pool::KEEP_FRAMES;

/// Most video memory kept in free buffers (bytes).
pub const FREE_BUDGET: u64 = 512 << 20;

/// Smallest size class.
pub const MIN_CLASS: u64 = 4096;

/// The usage of every attribute buffer: storage, copyable both ways, usable
/// for indirect draws and dispatches (headers) and as an index buffer.
pub const ATTR_USAGE: wgpu::BufferUsages = wgpu::BufferUsages::STORAGE
    .union(wgpu::BufferUsages::COPY_SRC)
    .union(wgpu::BufferUsages::COPY_DST)
    .union(wgpu::BufferUsages::INDIRECT)
    .union(wgpu::BufferUsages::INDEX);

/// The size class of a request: the next power of two from [`MIN_CLASS`],
/// or the request rounded to 256 bytes when that power of two would exceed
/// `max` (the device's `max_buffer_size`).
pub fn size_class(bytes: u64, max: u64) -> u64 {
    let class = bytes.max(MIN_CLASS).next_power_of_two();
    if class <= max {
        class
    } else {
        atd_core::geo::align_up(bytes)
    }
}

type Key = (u64, wgpu::BufferUsages);

#[derive(Debug, Default)]
struct State {
    /// Free buffers and the frame each became free in.
    free: HashMap<Key, Vec<(wgpu::Buffer, u64)>>,
    free_bytes: u64,
    frame: u64,
    created: usize,
}

#[derive(Debug, Default)]
struct Shared {
    /// Buffers dropped since the last `end_frame`, not yet reusable.
    returned: Mutex<Vec<wgpu::Buffer>>,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A pool of storage buffers, shared by the engine and its GEO operators.
/// Cloning gives another handle to the same pool.
#[derive(Debug, Clone)]
pub struct BufferPool {
    device: wgpu::Device,
    shared: Arc<Shared>,
}

/// A pooled buffer. Dropping it hands the buffer back to its pool.
#[derive(Debug)]
pub struct GeoBuffer {
    buffer: wgpu::Buffer,
    /// The pool to return to; `None` for buffers made outside a pool.
    home: Option<Arc<Shared>>,
}

impl GeoBuffer {
    /// Wraps a buffer that belongs to no pool (dropped normally).
    pub fn unpooled(buffer: wgpu::Buffer) -> Self {
        Self { buffer, home: None }
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Size of the buffer in bytes (its size class, at least what was asked).
    pub fn size(&self) -> u64 {
        self.buffer.size()
    }
}

impl std::ops::Deref for GeoBuffer {
    type Target = wgpu::Buffer;
    fn deref(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}

impl Drop for GeoBuffer {
    fn drop(&mut self) {
        if let Some(home) = &self.home {
            // `wgpu::Buffer` clones are reference counts; nothing allocates
            // once the list has grown to its working size.
            lock(&home.returned).push(self.buffer.clone());
        }
    }
}

impl BufferPool {
    pub fn new(device: wgpu::Device) -> Self {
        Self {
            device,
            shared: Arc::default(),
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// A buffer of at least `bytes` with `usage`, reused when one of its size
    /// class is free. Its contents are whatever it last held.
    pub fn acquire(&self, bytes: u64, usage: wgpu::BufferUsages) -> GeoBuffer {
        let class = size_class(bytes, self.device.limits().max_buffer_size);
        let mut state = lock(&self.shared.state);
        if let Some((buffer, _)) = state.free.get_mut(&(class, usage)).and_then(Vec::pop) {
            state.free_bytes -= class;
            return GeoBuffer {
                buffer,
                home: Some(self.shared.clone()),
            };
        }
        state.created += 1;
        drop(state);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atd-geo"),
            size: class,
            usage,
            mapped_at_creation: false,
        });
        GeoBuffer {
            buffer,
            home: Some(self.shared.clone()),
        }
    }

    /// An attribute buffer ([`ATTR_USAGE`]) of at least `bytes`.
    pub fn attr(&self, bytes: u64) -> GeoBuffer {
        self.acquire(bytes, ATTR_USAGE)
    }

    /// Ends a frame (after `queue.submit`): buffers returned during it become
    /// reusable, then free buffers older than [`KEEP_FRAMES`] are dropped,
    /// then the oldest while free memory exceeds [`FREE_BUDGET`] (never those
    /// returned this frame, which the next frame reuses). Allocates nothing
    /// in steady state.
    pub fn end_frame(&self) {
        let mut state = lock(&self.shared.state);
        let now = state.frame;
        {
            let mut returned = lock(&self.shared.returned);
            for buffer in returned.drain(..) {
                let key = (buffer.size(), buffer.usage());
                state.free_bytes += key.0;
                state.free.entry(key).or_default().push((buffer, now));
            }
        }
        let mut dropped = 0;
        for ((bytes, _), list) in &mut state.free {
            let before = list.len();
            list.retain(|(_, at)| now - at < KEEP_FRAMES);
            dropped += (before - list.len()) as u64 * bytes;
        }
        state.free_bytes -= dropped;
        let mut trimmed = dropped > 0;
        while state.free_bytes > FREE_BUDGET {
            trimmed = true;
            let oldest = state
                .free
                .iter()
                .filter_map(|(k, list)| list.first().map(|(_, at)| (*at, *k)))
                .filter(|(at, _)| *at < now)
                .min_by_key(|(at, _)| *at);
            let Some((_, key)) = oldest else { break };
            if let Some(list) = state.free.get_mut(&key) {
                list.remove(0);
                state.free_bytes -= key.0;
            }
        }
        if trimmed {
            state.free.retain(|_, list| !list.is_empty());
        }
        state.frame += 1;
    }

    /// Buffers ever created (a steady-state frame must not raise it).
    pub fn created(&self) -> usize {
        lock(&self.shared.state).created
    }

    /// Buffers free for reuse.
    pub fn free_count(&self) -> usize {
        lock(&self.shared.state).free.values().map(Vec::len).sum()
    }

    /// Video memory held by free buffers, in bytes.
    pub fn free_bytes(&self) -> u64 {
        lock(&self.shared.state).free_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_classes_are_powers_of_two_within_the_limit() {
        assert_eq!(size_class(1, 1 << 30), 4096);
        assert_eq!(size_class(4097, 1 << 30), 8192);
        assert_eq!(size_class(32_000_000, 1 << 30), 1 << 25);
        // Over the device limit: the request itself, aligned.
        assert_eq!(size_class(600 << 20, 1 << 30), 1 << 30);
        assert_eq!(size_class(600 << 20, 900 << 20), 600 << 20);
        assert_eq!(size_class(1000, 2048), 1024);
    }
}
