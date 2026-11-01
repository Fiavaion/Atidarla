//! GPU time of whole submissions, for benches and spikes (PRD §8): a
//! timestamp is written by an empty compute pass submitted before the work
//! and another after it. The queue runs submissions in order, so the
//! difference is the GPU time of everything submitted in between (plus
//! scheduling gaps). Needs [`wgpu::Features::TIMESTAMP_QUERY`]. Reading the
//! result waits for the GPU: not for the live render thread.

/// A pair of timestamps around submitted work.
#[derive(Debug)]
pub struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Nanoseconds per timestamp tick.
    period: f32,
}

impl GpuTimer {
    /// `None` when the device lacks `TIMESTAMP_QUERY`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("lsr-timer"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lsr-timer"),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Self {
            set,
            resolve: buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            read: buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
            period: queue.get_timestamp_period(),
        })
    }

    fn mark(&self, device: &wgpu::Device, queue: &wgpu::Queue, index: u32, resolve: bool) {
        let mut encoder = device.create_command_encoder(&Default::default());
        drop(encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("lsr-timer"),
            timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                query_set: &self.set,
                beginning_of_pass_write_index: Some(index),
                end_of_pass_write_index: None,
            }),
        }));
        if resolve {
            encoder.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
            encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
        }
        queue.submit([encoder.finish()]);
    }

    /// Call before submitting the work to time.
    pub fn start(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.mark(device, queue, 0, false);
    }

    /// Call after submitting it: waits for the GPU and returns the
    /// milliseconds between the two marks.
    pub fn stop(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<f64, String> {
        self.mark(device, queue, 1, true);
        let (tx, rx) = std::sync::mpsc::channel();
        self.read
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let ticks = {
            let data = self
                .read
                .slice(..)
                .get_mapped_range()
                .map_err(|e| e.to_string())?;
            let t =
                |i: usize| u64::from_le_bytes(data[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
            t(1).saturating_sub(t(0))
        };
        self.read.unmap();
        Ok(ticks as f64 * f64::from(self.period) / 1e6)
    }
}
