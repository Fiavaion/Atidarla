//! GPU → CPU copies of attributes without blocking the render thread
//! (CLAUDE.md rule 1; design §2.3).
//!
//! 1. While cooking, [`Readback::record`] copies each GPU-only attribute into a
//!    pooled staging buffer in *this frame's* encoder, which captures this
//!    frame's values even if the source is overwritten later.
//! 2. After the frame is submitted (`after_submit`), [`Readback::start`] maps
//!    the staging buffers.
//! 3. [`Readback::receive`] finishes it: live, only once the GPU is done
//!    (it never waits); offline (`CookMode::Offline`), it waits, so frame `i`
//!    reads the values of frame `i` (`docs/render.md`). The values become the
//!    attributes' CPU copies ([`Attribute::cpu_bytes`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Attribute, BufferPool, GeoBuffer};
use crate::engine::CookMode;

/// Usage of staging buffers.
const STAGING: wgpu::BufferUsages =
    wgpu::BufferUsages::MAP_READ.union(wgpu::BufferUsages::COPY_DST);

/// Bytes copied and mapped for `attr` (its values, copy-aligned).
fn staged_bytes(attr: &Attribute) -> u64 {
    attr.bytes()
        .max(4)
        .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
}

/// A batch of attribute readbacks (see the module documentation).
#[derive(Debug, Default)]
pub struct Readback {
    items: Vec<(Attribute, GeoBuffer)>,
    /// Map callbacks that have run (successfully or not).
    mapped: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
    started: bool,
}

impl Readback {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing is recorded or in flight.
    pub fn is_idle(&self) -> bool {
        self.items.is_empty()
    }

    /// Records copies of `attr` (if it is only on the GPU; others already
    /// have a CPU copy) into staging buffers in `encoder`.
    pub fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        pool: &BufferPool,
        attr: &Attribute,
    ) {
        assert!(!self.started, "record before start");
        let Some(src) = attr.gpu_buffer() else { return };
        if attr.cpu_bytes().is_some() {
            return;
        }
        let bytes = staged_bytes(attr);
        let staging = pool.acquire(bytes, STAGING);
        encoder.copy_buffer_to_buffer(src, 0, &staging, 0, bytes);
        self.items.push((attr.clone(), staging));
    }

    /// Maps the staging buffers; call once the recording encoder has been
    /// submitted.
    pub fn start(&mut self) {
        if self.started || self.items.is_empty() {
            return;
        }
        self.started = true;
        self.mapped.store(0, Ordering::SeqCst);
        self.failed.store(0, Ordering::SeqCst);
        for (attr, staging) in &self.items {
            let (mapped, failed) = (self.mapped.clone(), self.failed.clone());
            staging
                .slice(..staged_bytes(attr))
                .map_async(wgpu::MapMode::Read, move |r| {
                    if r.is_err() {
                        failed.fetch_add(1, Ordering::SeqCst);
                    }
                    mapped.fetch_add(1, Ordering::SeqCst);
                });
        }
    }

    /// Whether every staging buffer is mapped.
    pub fn is_ready(&self) -> bool {
        self.started && self.mapped.load(Ordering::SeqCst) == self.items.len()
    }

    /// Finishes the readback if the data is there: live it checks without
    /// waiting, offline it waits for the GPU. Returns `Ok(true)` once the
    /// attributes have their CPU copies (the batch is then empty again).
    pub fn receive(&mut self, mode: CookMode, device: &wgpu::Device) -> Result<bool, String> {
        if !self.started {
            return Ok(self.items.is_empty());
        }
        let poll = if mode.is_offline() {
            wgpu::PollType::wait_indefinitely()
        } else {
            wgpu::PollType::Poll
        };
        device.poll(poll).map_err(|e| e.to_string())?;
        if !self.is_ready() {
            return Ok(false);
        }
        let failed = self.failed.load(Ordering::SeqCst) > 0;
        for (attr, staging) in self.items.drain(..) {
            if !failed {
                let mapped = staged_bytes(&attr);
                let data = staging
                    .slice(..mapped)
                    .get_mapped_range()
                    .map_err(|e| e.to_string())?;
                attr.set_cpu_copy(data[..attr.bytes() as usize].into());
            }
            staging.unmap();
        }
        self.started = false;
        if failed {
            return Err("mapping a readback buffer failed".into());
        }
        Ok(true)
    }
}

/// Reads an attribute's values to the CPU, waiting for the GPU (tests,
/// tools and offline code; never on the live render thread). Values already
/// on the CPU are returned as they are.
pub fn read_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pool: &BufferPool,
    attr: &Attribute,
) -> Result<Vec<u8>, String> {
    if let Some(cpu) = attr.cpu_bytes() {
        return Ok(cpu.to_vec());
    }
    let mut rb = Readback::new();
    let mut encoder = device.create_command_encoder(&Default::default());
    rb.record(&mut encoder, pool, attr);
    queue.submit([encoder.finish()]);
    rb.start();
    while !rb.receive(CookMode::Offline { fps: 60.0 }, device)? {}
    let out = attr
        .cpu_bytes()
        .map(<[u8]>::to_vec)
        .ok_or("the attribute has no values")?;
    Ok(out)
}

/// Reads a GPU buffer's first `bytes` bytes, waiting for the GPU (tests and
/// tools).
pub fn read_buffer_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    bytes: u64,
) -> Result<Vec<u8>, String> {
    let bytes = bytes.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atd-geo-read"),
        size: bytes,
        usage: STAGING,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, bytes);
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let out = staging
        .slice(..)
        .get_mapped_range()
        .map_err(|e| e.to_string())?
        .to_vec();
    staging.unmap();
    Ok(out)
}
