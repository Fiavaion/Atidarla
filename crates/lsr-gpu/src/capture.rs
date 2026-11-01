//! Asynchronous texture readback: copies a texture of a fixed size into one
//! of a few mappable buffers and hands the pixels over on a later frame.
//! Nothing here waits for the GPU (rule 1): when every buffer is in flight
//! the frame is skipped and counted. Used by Record (FR-S10) and by AI nodes
//! sending frames to a host (FR-A1). Buffers are made once, so steady-state
//! use allocates nothing (rule 2).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const IDLE: u8 = 0;
/// Copy recorded in a caller's encoder, not yet submitted.
const RECORDED: u8 = 1;
const MAPPING: u8 = 2;
const READY: u8 = 3;

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// Order of capture, to hand frames over in order.
    seq: u64,
}

/// Copies a texture of a fixed size (4 bytes a pixel) to the CPU
/// asynchronously, a few frames in flight.
pub struct Capture {
    slots: Vec<Slot>,
    width: u32,
    height: u32,
    padded: u32,
    seq: u64,
    /// Frames not captured because every buffer was in flight.
    pub skipped: u64,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("size", &(self.width, self.height))
            .field("skipped", &self.skipped)
            .finish_non_exhaustive()
    }
}

impl Capture {
    pub fn new(device: &wgpu::Device, width: u32, height: u32, slots: usize) -> Self {
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let slots = (0..slots.max(1))
            .map(|_| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("lsr-capture"),
                    size: u64::from(padded) * u64::from(height),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(IDLE)),
                seq: 0,
            })
            .collect();
        Self {
            slots,
            width,
            height,
            padded,
            seq: 0,
            skipped: 0,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Records a copy of `texture` into `encoder` using a free buffer, or
    /// skips the frame when none is free. Call [`Capture::after_submit`] once
    /// the encoder is submitted. Returns whether a copy was recorded.
    pub fn record(&mut self, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) -> bool {
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|s| s.state.load(Ordering::Acquire) == IDLE)
        else {
            self.skipped += 1;
            return false;
        };
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        slot.state.store(RECORDED, Ordering::Release);
        slot.seq = self.seq;
        self.seq += 1;
        true
    }

    /// Starts mapping every copy recorded since the last call; the encoder
    /// holding them must have been submitted.
    pub fn after_submit(&mut self) {
        for slot in &self.slots {
            if slot.state.load(Ordering::Acquire) != RECORDED {
                continue;
            }
            slot.state.store(MAPPING, Ordering::Release);
            let state = slot.state.clone();
            slot.buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| {
                    state.store(if r.is_ok() { READY } else { IDLE }, Ordering::Release);
                });
        }
    }

    /// Copies `texture` with its own submit; skips the frame when every
    /// buffer is busy.
    pub fn copy(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lsr-capture"),
        });
        if self.record(&mut encoder, texture) {
            queue.submit([encoder.finish()]);
            self.after_submit();
        }
    }

    /// Hands every finished copy to `sink` as tightly packed rows
    /// (`sink(row, y)`), oldest first, without waiting.
    pub fn poll(&mut self, device: &wgpu::Device, mut sink: impl FnMut(&[u8], u32)) {
        let _ = device.poll(wgpu::PollType::Poll);
        loop {
            let next = self
                .slots
                .iter()
                .filter(|s| s.state.load(Ordering::Acquire) == READY)
                .min_by_key(|s| s.seq);
            let Some(slot) = next else { break };
            if let Ok(data) = slot.buffer.slice(..).get_mapped_range() {
                let row = self.width as usize * 4;
                for (y, chunk) in data
                    .chunks(self.padded as usize)
                    .take(self.height as usize)
                    .enumerate()
                {
                    sink(&chunk[..row], y as u32);
                }
            }
            slot.buffer.unmap();
            slot.state.store(IDLE, Ordering::Release);
        }
    }

    /// Whether a finished copy is waiting for [`Capture::poll`].
    pub fn ready(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.state.load(Ordering::Acquire) == READY)
    }

    /// Whether copies are still in flight.
    pub fn busy(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.state.load(Ordering::Acquire) != IDLE)
    }
}
