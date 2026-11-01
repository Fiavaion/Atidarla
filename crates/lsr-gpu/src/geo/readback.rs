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
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{AttrType, Attribute, BufferPool, GeoBuffer, Geometry, Scalar};
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

    /// Like [`Self::receive`], but instead of copying the values on this
    /// thread it hands over the mapped staging buffers ([`Mapped`]), so a
    /// worker thread copies them (large readbacks would otherwise cost the
    /// render thread a memory copy). `Ok(None)` while not ready.
    pub fn take_mapped(
        &mut self,
        mode: CookMode,
        device: &wgpu::Device,
    ) -> Result<Option<Mapped>, String> {
        if !self.started {
            return Ok(None);
        }
        let poll = if mode.is_offline() {
            wgpu::PollType::wait_indefinitely()
        } else {
            wgpu::PollType::Poll
        };
        device.poll(poll).map_err(|e| e.to_string())?;
        if !self.is_ready() {
            return Ok(None);
        }
        self.started = false;
        let items = std::mem::take(&mut self.items);
        if self.failed.load(Ordering::SeqCst) > 0 {
            for (_, staging) in &items {
                staging.unmap();
            }
            return Err("mapping a readback buffer failed".into());
        }
        Ok(Some(Mapped { items }))
    }
}

/// Mapped readback buffers, to read on any thread ([`Readback::take_mapped`]).
/// Dropping it unmaps them and returns them to the pool.
#[derive(Debug)]
pub struct Mapped {
    items: Vec<(Attribute, GeoBuffer)>,
}

impl Mapped {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The attribute read back as item `i` (in recording order).
    pub fn attribute(&self, i: usize) -> &Attribute {
        &self.items[i].0
    }

    /// Copies item `i`'s values (on the calling thread).
    pub fn bytes(&self, i: usize) -> Result<Vec<u8>, String> {
        let (attr, staging) = &self.items[i];
        let data = staging
            .slice(..staged_bytes(attr))
            .get_mapped_range()
            .map_err(|e| e.to_string())?;
        Ok(data[..attr.bytes() as usize].to_vec())
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        for (_, staging) in &self.items {
            staging.unmap();
        }
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
        label: Some("lsr-geo-read"),
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

const IDLE: u8 = 0;
const RECORDED: u8 = 1;
const MAPPING: u8 = 2;
const READY: u8 = 3;

/// Bytes before the attribute values in a [`PointReadback`] staging buffer:
/// the start of the geometry's header (its live count).
const COUNT_BYTES: u64 = 16;

/// One staging buffer of a [`PointReadback`].
#[derive(Debug)]
struct RingSlot {
    buffer: Option<wgpu::Buffer>,
    state: Arc<AtomicU8>,
    frame: u64,
    since: Instant,
    elements: u32,
    ty: AttrType,
    counted: bool,
}

/// What [`PointReadback::record`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// A copy is in the encoder; the values arrive after
    /// [`PointReadback::start`] and a later [`PointReadback::receive`].
    Copy,
    /// The attribute had a CPU copy: its values are already read.
    Now,
    /// Every staging buffer is in flight; record again next frame.
    Busy,
    /// The geometry has no such point attribute.
    Missing,
}

/// Reads the first elements of one point attribute (and the live point
/// count) back to the CPU for a GPU→SIG operator (PRD A65) through a ring of
/// three staging buffers, as Analyze and Pixel Map do for images: a copy
/// recorded into the frame's encoder, mapped after the frame is submitted
/// ([`Self::start`]) and read on a later cook without waiting
/// ([`Self::receive`]), so live values trail the points by one frame or
/// two. Offline, [`Self::finish_now`] submits and waits instead.
///
/// After warm-up nothing is allocated: staging buffers only grow (to a
/// power of two) and the value buffer is reused.
#[derive(Debug)]
pub struct PointReadback {
    slots: [RingSlot; 3],
    bytes: Vec<u8>,
    count: u32,
    ty: AttrType,
    frame: Option<u64>,
}

impl Default for PointReadback {
    fn default() -> Self {
        let slot = || RingSlot {
            buffer: None,
            state: Arc::new(AtomicU8::new(IDLE)),
            frame: 0,
            since: Instant::now(),
            elements: 0,
            ty: AttrType::F32,
            counted: false,
        };
        Self {
            slots: [slot(), slot(), slot()],
            bytes: Vec::new(),
            count: 0,
            ty: AttrType::F32,
            frame: None,
        }
    }
}

impl PointReadback {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a copy of the first `max` elements of point attribute `name`
    /// of `geo` (and its GPU live count) into a free staging buffer; `frame`
    /// tags the values. A CPU-resident attribute is read at once.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        geo: &Geometry,
        name: &str,
        max: u32,
        frame: u64,
    ) -> Recorded {
        let Some(attr) = geo.point(name) else {
            return Recorded::Missing;
        };
        let ty = attr.ty();
        let stride = u64::from(ty.stride());
        let elements = geo.points().min(max).min(attr.len());
        if let Some(cpu) = attr.cpu_bytes() {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&cpu[..(u64::from(elements) * stride) as usize]);
            self.count = elements;
            self.ty = ty;
            self.frame = Some(frame);
            return Recorded::Now;
        }
        let Some(src) = attr.gpu_buffer() else {
            return Recorded::Missing;
        };
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|s| s.state.load(Ordering::Acquire) == IDLE)
        else {
            return Recorded::Busy;
        };
        let needed = COUNT_BYTES + u64::from(elements) * stride;
        if slot.buffer.as_ref().is_none_or(|b| b.size() < needed) {
            slot.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lsr-point-readback"),
                size: needed.next_power_of_two(),
                usage: STAGING,
                mapped_at_creation: false,
            }));
        }
        let dst = slot.buffer.as_ref().expect("created above");
        let header = geo.header().filter(|_| geo.is_gpu_counted());
        if let Some(header) = header {
            encoder.copy_buffer_to_buffer(header, 0, dst, 0, COUNT_BYTES);
        }
        if elements > 0 {
            encoder.copy_buffer_to_buffer(src, 0, dst, COUNT_BYTES, needed - COUNT_BYTES);
        }
        slot.frame = frame;
        slot.since = Instant::now();
        slot.elements = elements;
        slot.ty = ty;
        slot.counted = header.is_some();
        slot.state.store(RECORDED, Ordering::Release);
        Recorded::Copy
    }

    /// Maps the recorded staging buffers; call once the encoder holding the
    /// copies has been submitted (`after_submit`).
    pub fn start(&mut self) {
        for slot in &self.slots {
            if slot.state.load(Ordering::Acquire) != RECORDED {
                continue;
            }
            let Some(buffer) = &slot.buffer else { continue };
            slot.state.store(MAPPING, Ordering::Release);
            let state = slot.state.clone();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                state.store(if r.is_ok() { READY } else { IDLE }, Ordering::Release);
            });
        }
    }

    /// Takes the newest mapped values without waiting (it only polls the
    /// device). Returns whether newer values arrived.
    pub fn receive(&mut self, device: &wgpu::Device) -> Result<bool, String> {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|e| e.to_string())?;
        self.take_ready()
    }

    /// Offline: submits the encoder recorded so far (the points' producers
    /// and the copy), maps and waits, so the values are this frame's.
    pub fn finish_now(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<bool, String> {
        let done = std::mem::replace(encoder, device.create_command_encoder(&Default::default()));
        queue.submit([done.finish()]);
        self.start();
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        self.take_ready()
    }

    fn take_ready(&mut self) -> Result<bool, String> {
        let mut fresh = false;
        let mut failed = None;
        for slot in &mut self.slots {
            if slot.state.load(Ordering::Acquire) != READY {
                continue;
            }
            let Some(buffer) = &slot.buffer else { continue };
            if self.frame.is_none_or(|f| slot.frame >= f) {
                match buffer.slice(..).get_mapped_range() {
                    Ok(data) => {
                        let start = COUNT_BYTES as usize;
                        let len = slot.elements as usize * slot.ty.stride() as usize;
                        self.bytes.clear();
                        self.bytes.extend_from_slice(&data[start..start + len]);
                        self.count = if slot.counted {
                            u32::from_le_bytes([data[0], data[1], data[2], data[3]])
                                .min(slot.elements)
                        } else {
                            slot.elements
                        };
                        self.ty = slot.ty;
                        self.frame = Some(slot.frame);
                        fresh = true;
                    }
                    Err(e) => failed = Some(e.to_string()),
                }
            }
            buffer.unmap();
            slot.state.store(IDLE, Ordering::Release);
        }
        match failed {
            Some(e) => Err(format!("reading points back failed: {e}")),
            None => Ok(fresh),
        }
    }

    /// Whether a copy is recorded or in flight.
    pub fn busy(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.state.load(Ordering::Acquire) != IDLE)
    }

    /// How long the oldest copy in flight has been waiting.
    pub fn oldest(&self) -> Option<Duration> {
        self.slots
            .iter()
            .filter(|s| s.state.load(Ordering::Acquire) != IDLE)
            .map(|s| s.since.elapsed())
            .max()
    }

    /// Forgets the values read (the input went away).
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.count = 0;
        self.frame = None;
    }

    /// Points in the latest values (at most the `max` recorded).
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Type of the attribute read.
    pub fn ty(&self) -> AttrType {
        self.ty
    }

    /// Frame the latest values were recorded on, if any arrived.
    pub fn frame(&self) -> Option<u64> {
        self.frame
    }

    /// Component `c` of point `i` of the latest values as `f32` (integer
    /// attributes convert); 0 out of range.
    pub fn value(&self, i: u32, c: u8) -> f32 {
        if i >= self.count || c >= self.ty.comps() {
            return 0.0;
        }
        let at = i as usize * self.ty.stride() as usize + usize::from(c) * 4;
        let Some(b) = self.bytes.get(at..at + 4) else {
            return 0.0;
        };
        let b = [b[0], b[1], b[2], b[3]];
        match self.ty.scalar {
            Scalar::F32 => f32::from_le_bytes(b),
            Scalar::I32 => i32::from_le_bytes(b) as f32,
            Scalar::U32 => u32::from_le_bytes(b) as f32,
        }
    }
}
