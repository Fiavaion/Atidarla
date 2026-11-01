//! Live audio capture (SIG Audio In).
//!
//! [`AudioIn::open`] returns at once. A dedicated thread enumerates and opens
//! the device and owns the cpal stream. The device callback converts samples
//! to `f32` and pushes them, interleaved, into a lock-free single-producer
//! single-consumer [`Ring`]. The cook drains the ring without locking or
//! allocating (CLAUDE.md rules 1 and 2). Dropping the handle stops the
//! stream without waiting for it.
//!
//! The device name [`TEST_TONE`] opens a built-in source instead of a
//! device: a stereo 440 Hz sine (left) and 660 Hz sine (right) at 48 kHz,
//! paced in real time. It serves tests, CI runners without audio hardware,
//! and machines without a microphone.

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// Device name of the built-in test source.
pub const TEST_TONE: &str = "test-tone";

/// Ring capacity in seconds of audio.
const RING_SECONDS: f64 = 2.0;

/// A lock-free single-producer, single-consumer ring of `f32` samples.
#[derive(Debug)]
pub struct Ring {
    buf: Box<[AtomicU32]>,
    /// Samples written / read since creation (monotonic).
    written: AtomicUsize,
    read: AtomicUsize,
    /// Samples dropped because the reader fell behind.
    overruns: AtomicU64,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: (0..capacity.max(1)).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            overruns: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Producer: appends `samples`, dropping what does not fit.
    pub fn push(&self, samples: impl IntoIterator<Item = f32>) {
        let w = self.written.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        let free = self.capacity() - (w - r);
        let mut n = 0;
        for s in samples {
            if n == free {
                self.overruns.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            self.buf[(w + n) % self.capacity()].store(s.to_bits(), Ordering::Relaxed);
            n += 1;
        }
        self.written.store(w + n, Ordering::Release);
    }

    /// Samples ready to read.
    pub fn available(&self) -> usize {
        self.written.load(Ordering::Acquire) - self.read.load(Ordering::Relaxed)
    }

    /// Consumer: fills `out` from the oldest samples; returns how many.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let r = self.read.load(Ordering::Relaxed);
        let n = self.available().min(out.len());
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = f32::from_bits(self.buf[(r + i) % self.capacity()].load(Ordering::Relaxed));
        }
        self.read.store(r + n, Ordering::Release);
        n
    }

    /// Consumer: discards up to `n` of the oldest samples.
    pub fn skip(&self, n: usize) {
        let r = self.read.load(Ordering::Relaxed);
        self.read
            .store(r + n.min(self.available()), Ordering::Release);
    }

    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }
}

/// The opened stream's format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
    /// The device actually opened (the default device's name for "").
    pub device: String,
}

#[derive(Debug, Default)]
struct Shared {
    format: OnceLock<Format>,
    ring: OnceLock<Arc<Ring>>,
    error: OnceLock<String>,
}

/// Handle to a capture running on its own thread.
#[derive(Debug)]
pub struct AudioIn {
    shared: Arc<Shared>,
    _stop: Sender<()>,
}

impl AudioIn {
    /// Starts capturing from `device` ("" for the system default, or
    /// [`TEST_TONE`]). Never blocks; the format appears once the device is
    /// open, or [`AudioIn::error`] reports why it could not open.
    pub fn open(device: &str) -> Self {
        let shared = Arc::new(Shared::default());
        let (stop, stopped) = channel();
        let device = device.to_owned();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("lsr-audio-in".into())
            .spawn(move || {
                let result = if device == TEST_TONE {
                    run_tone(&thread_shared, &stopped)
                } else {
                    run_device(&device, &thread_shared, &stopped)
                };
                if let Err(e) = result {
                    log::warn!("audio in: {e}");
                    let _ = thread_shared.error.set(e);
                }
            });
        if let Err(e) = spawned {
            let _ = shared.error.set(format!("cannot start audio thread: {e}"));
        }
        Self {
            shared,
            _stop: stop,
        }
    }

    pub fn format(&self) -> Option<&Format> {
        self.shared.format.get()
    }

    pub fn error(&self) -> Option<&str> {
        self.shared.error.get().map(String::as_str)
    }

    /// The interleaved sample ring, once the device is open.
    pub fn ring(&self) -> Option<&Ring> {
        self.shared.ring.get().map(Arc::as_ref)
    }
}

fn ring_for(shared: &Shared, rate: u32, channels: u16) -> Arc<Ring> {
    let capacity = (f64::from(rate) * RING_SECONDS) as usize * usize::from(channels);
    shared
        .ring
        .get_or_init(|| Arc::new(Ring::new(capacity)))
        .clone()
}

/// Waits for the handle to drop, or for `timeout`. True when stopped.
fn stopped_within(stop: &Receiver<()>, timeout: Duration) -> bool {
    !matches!(stop.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
}

fn run_tone(shared: &Shared, stop: &Receiver<()>) -> Result<(), String> {
    const RATE: u32 = 48_000;
    let ring = ring_for(shared, RATE, 2);
    let _ = shared.format.set(Format {
        rate: RATE,
        channels: 2,
        device: TEST_TONE.into(),
    });
    let start = Instant::now();
    let mut frames: u64 = 0;
    loop {
        let due = (start.elapsed().as_secs_f64() * f64::from(RATE)) as u64;
        let t0 = frames;
        ring.push((t0..due).flat_map(|i| {
            let t = i as f64 / f64::from(RATE);
            let l = (t * 440.0 * std::f64::consts::TAU).sin() as f32 * 0.5;
            let r = (t * 660.0 * std::f64::consts::TAU).sin() as f32 * 0.5;
            [l, r]
        }));
        frames = due.max(frames);
        if stopped_within(stop, Duration::from_millis(5)) {
            return Ok(());
        }
    }
}

fn run_device(name: &str, shared: &Shared, stop: &Receiver<()>) -> Result<(), String> {
    let host = cpal::default_host();
    let device = if name.is_empty() {
        host.default_input_device()
            .ok_or_else(|| "no default audio input device".to_owned())?
    } else {
        host.input_devices()
            .map_err(|e| format!("listing audio inputs: {e}"))?
            .find(|d| d.to_string() == name)
            .ok_or_else(|| format!("no audio input named {name:?}"))?
    };
    let supported = device
        .default_input_config()
        .map_err(|e| format!("{device}: {e}"))?;
    let config = supported.config();
    let format = supported.sample_format();
    let ring = ring_for(shared, config.sample_rate, config.channels);
    let stream = device
        .build_input_stream_raw(
            config,
            format,
            move |data, _| match data.sample_format() {
                SampleFormat::F32 => push_as::<f32>(data, &ring),
                SampleFormat::F64 => push_as::<f64>(data, &ring),
                SampleFormat::I16 => push_as::<i16>(data, &ring),
                SampleFormat::I32 => push_as::<i32>(data, &ring),
                SampleFormat::U16 => push_as::<u16>(data, &ring),
                SampleFormat::I8 => push_as::<i8>(data, &ring),
                SampleFormat::U8 => push_as::<u8>(data, &ring),
                _ => {}
            },
            |e| log::warn!("audio in stream: {e}"),
            Some(Duration::from_secs(5)),
        )
        .map_err(|e| format!("{device}: {e}"))?;
    stream.play().map_err(|e| format!("{device}: {e}"))?;
    let _ = shared.format.set(Format {
        rate: supported.sample_rate(),
        channels: supported.channels(),
        device: device.to_string(),
    });
    // Keep the stream alive until the handle drops.
    while !stopped_within(stop, Duration::from_secs(3600)) {}
    drop(stream);
    Ok(())
}

fn push_as<T>(data: &cpal::Data, ring: &Ring)
where
    T: SizedSample,
    f32: FromSample<T>,
{
    if let Some(samples) = data.as_slice::<T>() {
        ring.push(
            samples
                .iter()
                .map(|s| <f32 as FromSample<T>>::from_sample_(*s)),
        );
    }
}

/// Names of the available input devices (blocks while the host enumerates,
/// so call it off the render thread). [`TEST_TONE`] is always listed.
pub fn input_devices() -> Vec<String> {
    let mut names = vec![TEST_TONE.to_owned()];
    if let Ok(devices) = cpal::default_host().input_devices() {
        names.extend(devices.map(|d| d.to_string()));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_order_and_drops_overruns() {
        let ring = Ring::new(4);
        ring.push([1.0, 2.0, 3.0]);
        assert_eq!(ring.available(), 3);
        let mut out = [0.0; 2];
        assert_eq!(ring.pop(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);
        ring.push([4.0, 5.0, 6.0, 7.0]);
        assert_eq!(ring.available(), 4);
        assert_eq!(ring.overruns(), 1);
        let mut out = [0.0; 8];
        assert_eq!(ring.pop(&mut out), 4);
        assert_eq!(&out[..4], &[3.0, 4.0, 5.0, 6.0]);
        ring.push([8.0, 9.0]);
        ring.skip(1);
        assert_eq!(ring.pop(&mut out), 1);
        assert_eq!(out[0], 9.0);
        assert_eq!(ring.available(), 0);
    }

    #[test]
    fn ring_crosses_threads() {
        let ring = Arc::new(Ring::new(1024));
        let producer = ring.clone();
        let t = std::thread::spawn(move || {
            let mut next = 0u32;
            while next < 100_000 {
                let free = producer.capacity() - producer.available();
                let n = free.min(100_000 - next as usize) as u32;
                producer.push((next..next + n).map(|i| i as f32));
                next += n;
            }
        });
        let mut expected = 0u32;
        let mut buf = [0.0f32; 64];
        while expected < 100_000 {
            let n = ring.pop(&mut buf);
            for v in &buf[..n] {
                assert_eq!(*v, expected as f32);
                expected += 1;
            }
        }
        t.join().unwrap();
        assert_eq!(ring.overruns(), 0);
    }

    #[test]
    fn test_tone_streams_in_real_time() {
        let input = AudioIn::open(TEST_TONE);
        let start = Instant::now();
        while input.ring().is_none_or(|r| r.available() < 4800) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "tone never arrived"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let format = input.format().unwrap();
        assert_eq!((format.rate, format.channels), (48_000, 2));
        let mut buf = vec![0.0f32; 4800];
        input.ring().unwrap().pop(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((0.45..=0.5).contains(&peak), "peak {peak}");
        assert!(input.error().is_none());
    }

    #[test]
    fn missing_devices_report_without_blocking() {
        let start = Instant::now();
        let input = AudioIn::open("no such device, surely");
        assert!(start.elapsed() < Duration::from_millis(100));
        while input.error().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no error reported"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(input.format().is_none());
    }
}
