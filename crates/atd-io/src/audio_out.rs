//! Live audio playback (SIG Audio Out).
//!
//! [`AudioOut::open`] returns at once. A dedicated thread opens the device
//! and owns the cpal stream. The cook pushes interleaved `f32` frames at
//! the device's rate into a lock-free [`Ring`] (no locks, no allocation,
//! rules 1 and 2), and the device callback pops them, playing silence
//! when the ring runs dry. [`Resampler`] converts from the source rate.
//!
//! The device name [`NULL_OUTPUT`] plays nowhere: a thread consumes 48 kHz
//! stereo in real time and keeps the last few seconds, for tests and
//! machines without audio output.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

pub use crate::audio_in::Ring;

/// Seconds of audio the null output keeps for inspection.
pub const CAPTURE_SECONDS: usize = 5;

/// Device name of the silent test output.
pub const NULL_OUTPUT: &str = "null";

/// Ring capacity in seconds; the cook keeps at most [`MAX_LATENCY`] queued.
const RING_SECONDS: f64 = 1.0;
/// Most audio queued ahead of the device before old samples are dropped.
pub const MAX_LATENCY: f64 = 0.15;

/// The opened stream's format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
    pub device: String,
}

#[derive(Debug, Default)]
struct Shared {
    format: OnceLock<Format>,
    ring: OnceLock<Arc<Ring>>,
    error: OnceLock<String>,
    /// Frames the device wanted but the ring did not have.
    underruns: AtomicU64,
    /// Null output only: the last [`CAPTURE_SECONDS`], interleaved.
    captured: Mutex<Vec<f32>>,
}

/// Handle to playback running on its own thread.
#[derive(Debug)]
pub struct AudioOut {
    shared: Arc<Shared>,
    _stop: Sender<()>,
}

impl AudioOut {
    /// Starts opening `device` ("" for the default output, or
    /// [`NULL_OUTPUT`]). Never blocks.
    pub fn open(device: &str) -> Self {
        let shared = Arc::new(Shared::default());
        let (stop, stopped) = channel::<()>();
        let thread_shared = shared.clone();
        let device = device.to_owned();
        let spawned = std::thread::Builder::new()
            .name("atd-audio-out".into())
            .spawn(move || {
                let result = if device == NULL_OUTPUT {
                    run_null(&thread_shared, &stopped)
                } else {
                    run_device(&device, &thread_shared, &stopped)
                };
                if let Err(e) = result {
                    log::warn!("audio out: {e}");
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

    /// Where interleaved frames at the device rate go.
    pub fn ring(&self) -> Option<&Ring> {
        self.shared.ring.get().map(|r| &**r)
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Null output only: a copy of the last [`CAPTURE_SECONDS`] played.
    pub fn captured(&self) -> Vec<f32> {
        self.shared
            .captured
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }
}

fn ring_for(shared: &Shared, rate: u32, channels: u16) -> Arc<Ring> {
    let capacity = (f64::from(rate) * RING_SECONDS) as usize * usize::from(channels);
    shared
        .ring
        .get_or_init(|| Arc::new(Ring::new(capacity)))
        .clone()
}

fn stopped_within(stop: &Receiver<()>, wait: Duration) -> bool {
    !matches!(stop.recv_timeout(wait), Err(RecvTimeoutError::Timeout))
}

fn run_null(shared: &Shared, stop: &Receiver<()>) -> Result<(), String> {
    const RATE: u32 = 48_000;
    let ring = ring_for(shared, RATE, 2);
    let _ = shared.format.set(Format {
        rate: RATE,
        channels: 2,
        device: NULL_OUTPUT.into(),
    });
    let start = Instant::now();
    let mut played = 0u64;
    let mut buf = vec![0.0f32; 2 * 4800];
    loop {
        let due = (start.elapsed().as_secs_f64() * f64::from(RATE)) as u64;
        let frames = (due - played).min(4800) as usize;
        if frames > 0 {
            let got = ring.pop(&mut buf[..frames * 2]);
            buf[got..frames * 2].fill(0.0);
            if got < frames * 2 {
                shared
                    .underruns
                    .fetch_add(((frames * 2 - got) / 2) as u64, Ordering::Relaxed);
            }
            if let Ok(mut c) = shared.captured.lock() {
                c.extend_from_slice(&buf[..frames * 2]);
                let keep = RATE as usize * 2 * CAPTURE_SECONDS;
                if c.len() > keep {
                    let drop = c.len() - keep;
                    c.drain(..drop);
                }
            }
            played += frames as u64;
        }
        if stopped_within(stop, Duration::from_millis(5)) {
            return Ok(());
        }
    }
}

fn run_device(name: &str, shared: &Arc<Shared>, stop: &Receiver<()>) -> Result<(), String> {
    let host = cpal::default_host();
    let device = if name.is_empty() {
        host.default_output_device()
            .ok_or_else(|| "no default audio output device".to_owned())?
    } else {
        host.output_devices()
            .map_err(|e| format!("listing audio outputs: {e}"))?
            .find(|d| d.to_string() == name)
            .ok_or_else(|| format!("no audio output named {name:?}"))?
    };
    let supported = device
        .default_output_config()
        .map_err(|e| format!("{device}: {e}"))?;
    let config = supported.config();
    let format = supported.sample_format();
    let ring = ring_for(shared, config.sample_rate, config.channels);
    let stride = usize::from(config.channels.max(1));
    let cb_shared = shared.clone();
    // Scratch for format conversion; sized once, grown only if the device
    // asks for a bigger buffer than before.
    let mut scratch: Vec<f32> = Vec::with_capacity(8192);
    let stream = device
        .build_output_stream_raw(
            config,
            format,
            move |data, _| {
                let n = data.len();
                if scratch.len() < n {
                    scratch.resize(n, 0.0);
                }
                let got = ring.pop(&mut scratch[..n]);
                scratch[got..n].fill(0.0);
                if got < n {
                    cb_shared
                        .underruns
                        .fetch_add(((n - got) / stride) as u64, Ordering::Relaxed);
                }
                match data.sample_format() {
                    SampleFormat::F32 => write_as::<f32>(data, &scratch[..n]),
                    SampleFormat::F64 => write_as::<f64>(data, &scratch[..n]),
                    SampleFormat::I16 => write_as::<i16>(data, &scratch[..n]),
                    SampleFormat::I32 => write_as::<i32>(data, &scratch[..n]),
                    SampleFormat::U16 => write_as::<u16>(data, &scratch[..n]),
                    SampleFormat::I8 => write_as::<i8>(data, &scratch[..n]),
                    SampleFormat::U8 => write_as::<u8>(data, &scratch[..n]),
                    _ => {}
                }
            },
            |e| log::warn!("audio out stream: {e}"),
            Some(Duration::from_secs(5)),
        )
        .map_err(|e| format!("{device}: {e}"))?;
    stream.play().map_err(|e| format!("{device}: {e}"))?;
    let _ = shared.format.set(Format {
        rate: supported.sample_rate(),
        channels: supported.channels(),
        device: device.to_string(),
    });
    while !stopped_within(stop, Duration::from_secs(3600)) {}
    drop(stream);
    Ok(())
}

fn write_as<T>(data: &mut cpal::Data, src: &[f32])
where
    T: SizedSample + FromSample<f32>,
{
    if let Some(out) = data.as_slice_mut::<T>() {
        for (o, s) in out.iter_mut().zip(src) {
            *o = T::from_sample_(s.clamp(-1.0, 1.0));
        }
    }
}

/// Names of the available output devices (blocks while the host
/// enumerates; call it off the render thread). [`NULL_OUTPUT`] is listed.
pub fn output_devices() -> Vec<String> {
    let mut names = vec![NULL_OUTPUT.to_owned()];
    if let Ok(devices) = cpal::default_host().output_devices() {
        names.extend(devices.map(|d| d.to_string()));
    }
    names
}

/// Streaming linear-interpolation resampler for interleaved audio. Keeps
/// its position and the last input frame between blocks, so consecutive
/// blocks join without clicks. Allocation-free after the first block.
#[derive(Debug, Default)]
pub struct Resampler {
    channels: usize,
    /// Position of the next output frame, in input frames, relative to the
    /// start of the next block (−1 ≤ pos: between `last` and frame 0).
    pos: f64,
    last: Vec<f32>,
    primed: bool,
}

impl Resampler {
    /// Converts `input` (interleaved, `channels` per frame, at `from` Hz)
    /// to `to` Hz, appending to `out`.
    pub fn process(
        &mut self,
        input: &[f32],
        channels: usize,
        from: f64,
        to: f64,
        out: &mut Vec<f32>,
    ) {
        let channels = channels.max(1);
        if channels != self.channels {
            *self = Self {
                channels,
                last: vec![0.0; channels],
                ..Default::default()
            };
        }
        let frames = input.len() / channels;
        if frames == 0 || from <= 0.0 || to <= 0.0 {
            return;
        }
        if !self.primed {
            self.last.copy_from_slice(&input[..channels]);
            self.pos = 0.0;
            self.primed = true;
        }
        let step = from / to;
        let frame = |i: isize, c: usize| -> f32 {
            if i < 0 {
                self.last[c]
            } else {
                input[i as usize * channels + c]
            }
        };
        // Interpolate between frame floor(pos) and the next, which must
        // exist in this block.
        while self.pos < (frames - 1) as f64 {
            let i = self.pos.floor() as isize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..channels {
                let a = frame(i, c);
                let b = frame(i + 1, c);
                out.push(a + (b - a) * t);
            }
            self.pos += step;
        }
        // Carry over: the last frame becomes frame −1 of the next block.
        self.last
            .copy_from_slice(&input[(frames - 1) * channels..frames * channels]);
        self.pos -= frames as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_rate_and_continuity() {
        // 1 kHz sine at 44.1 kHz, in uneven blocks, to 48 kHz.
        let src: Vec<f32> = (0..44_100)
            .map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / 44_100.0).sin() as f32)
            .collect();
        let mut r = Resampler::default();
        let mut out = Vec::new();
        for chunk in src.chunks(735 + 17) {
            r.process(chunk, 1, 44_100.0, 48_000.0, &mut out);
        }
        assert!(
            (out.len() as i64 - 48_000).abs() <= 2,
            "{} frames",
            out.len()
        );
        // Compare with the ideal 1 kHz sine at 48 kHz.
        let worst = out
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let ideal = (std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0).sin() as f32;
                (v - ideal).abs()
            })
            .fold(0.0f32, f32::max);
        assert!(worst < 0.01, "max error {worst}");
    }

    #[test]
    fn resampler_passes_equal_rates_through() {
        let mut r = Resampler::default();
        let mut out = Vec::new();
        r.process(
            &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0],
            2,
            48_000.0,
            48_000.0,
            &mut out,
        );
        r.process(&[4.0, 40.0], 2, 48_000.0, 48_000.0, &mut out);
        assert_eq!(out, [1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);
        r.process(&[5.0, 50.0], 2, 48_000.0, 48_000.0, &mut out);
        assert_eq!(&out[6..], [4.0, 40.0], "one frame of latency, no gaps");
    }

    #[test]
    fn null_output_plays_what_is_pushed() {
        let out = AudioOut::open(NULL_OUTPUT);
        let start = Instant::now();
        while out.ring().is_none() {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        let fmt = out.format().unwrap().clone();
        assert_eq!((fmt.rate, fmt.channels), (48_000, 2));
        let ring = out.ring().unwrap();
        let tone: Vec<f32> = (0..4800).flat_map(|_| [0.5, -0.5]).collect();
        ring.push(tone.iter().copied());
        std::thread::sleep(Duration::from_millis(300));
        let played = out.captured();
        let loud = played
            .chunks(2)
            .filter(|f| f[0] == 0.5 && f[1] == -0.5)
            .count();
        assert_eq!(loud, 4800, "every pushed frame was played once");
        assert!(
            out.underruns() > 0,
            "silence after the tone counts as underrun"
        );
    }
}
