//! Audio operators: file playback and spectrum analysis.
//!
//! Audio arrives as audio-rate [`Channels`] blocks: each engine frame carries
//! the samples played since the previous frame. Decoding happens on a worker
//! thread, never on the cook path (CLAUDE.md rule 1).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_gpu::engine::{Channels, SigCtx, SigOp};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Most samples emitted in one frame (protects against long stalls).
const MAX_BLOCK: usize = 16_384;

/// A decoded clip: one `Vec` per channel.
#[derive(Debug)]
pub struct Clip {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl Clip {
    fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}

/// Decodes a WAV file (PCM integer or float) into normalised samples.
pub fn decode_wav(path: &std::path::Path) -> Result<Clip, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    let n = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
        }
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut channels = vec![Vec::with_capacity(interleaved.len() / n); n];
    for frame in interleaved.chunks_exact(n) {
        for (c, s) in frame.iter().enumerate() {
            channels[c].push(*s);
        }
    }
    Ok(Clip {
        rate: spec.sample_rate,
        channels,
    })
}

/// Plays a WAV file as audio-rate channels `left`/`right` (or `mono`).
#[derive(Default)]
pub struct AudioFileIn {
    path: String,
    clip: Option<Arc<Clip>>,
    loading: Option<Receiver<Result<Clip, String>>>,
    error: Option<String>,
    /// Playhead in frames.
    pos: f64,
}

impl SigOp for AudioFileIn {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let path = p.str("file");
        if path != self.path {
            // New file: decode on a worker thread; keep cooking meanwhile.
            self.path = path.to_owned();
            self.clip = None;
            self.error = None;
            self.pos = 0.0;
            self.loading = None;
            if !path.is_empty() {
                let (tx, rx) = channel();
                let file = PathBuf::from(path);
                std::thread::Builder::new()
                    .name("atd-audio-decode".into())
                    .spawn(move || {
                        let _ = tx.send(decode_wav(&file));
                    })
                    .map_err(|e| e.to_string())?;
                self.loading = Some(rx);
            }
        }
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(Ok(clip)) => {
                    self.clip = Some(Arc::new(clip));
                    self.loading = None;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.loading = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("decoder thread failed".into());
                    self.loading = None;
                }
            }
        }
        if let Some(e) = &self.error {
            ctx.output.set_block(&[], 0.0, 0);
            return Err(e.clone());
        }
        let Some(clip) = &self.clip else {
            ctx.output.set_block(&[], 0.0, 0);
            return Ok(());
        };

        let names: &[&str] = if clip.channels.len() == 1 {
            &["mono"]
        } else {
            &["left", "right"]
        };
        let frames = clip.frames();
        let count = if p.bool("play") && frames > 0 {
            ((ctx.dt * f64::from(clip.rate)).round() as usize).min(MAX_BLOCK)
        } else {
            0
        };
        ctx.output.set_block(names, clip.rate as f32, count);
        let gain = p.f32("gain");
        let looping = p.bool("loop");
        let start = self.pos as usize;
        for (c, _) in names.iter().enumerate() {
            let src = &clip.channels[c.min(clip.channels.len() - 1)];
            for (i, out) in ctx.output.channel_mut(c).iter_mut().enumerate() {
                let mut idx = start + i;
                if looping {
                    idx %= frames;
                }
                *out = src.get(idx).copied().unwrap_or(0.0) * gain;
            }
        }
        self.pos += count as f64;
        if looping && frames > 0 {
            self.pos %= frames as f64;
        } else {
            self.pos = self.pos.min(frames as f64);
        }
        Ok(())
    }
}

const BAND_NAMES: [&str; 32] = [
    "band1", "band2", "band3", "band4", "band5", "band6", "band7", "band8", "band9", "band10",
    "band11", "band12", "band13", "band14", "band15", "band16", "band17", "band18", "band19",
    "band20", "band21", "band22", "band23", "band24", "band25", "band26", "band27", "band28",
    "band29", "band30", "band31", "band32",
];

pub const FFT_SIZES: &[&str] = &["512", "1024", "2048", "4096"];

/// A sliding analysis window over an audio-rate input with a cached FFT.
#[derive(Default)]
pub struct FftWindow {
    size: usize,
    fft: Option<Arc<dyn Fft<f32>>>,
    window: Vec<f32>,
    /// Most recent `size` mono samples, oldest first.
    ring: Vec<f32>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Sample rate of the input, once known.
    pub rate: f32,
}

impl FftWindow {
    /// Sets the window size (allocates only when it changes).
    pub fn prepare(&mut self, size: usize) {
        if self.size == size && self.fft.is_some() {
            return;
        }
        self.size = size;
        let fft = FftPlanner::new().plan_fft_forward(size);
        self.scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        self.fft = Some(fft);
        // Hann window.
        self.window = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
            .collect();
        self.ring = vec![0.0; size];
        self.buf = vec![Complex32::default(); size];
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Appends the input's samples (mixed to mono), keeping the last `size`.
    pub fn push(&mut self, input: &Channels) {
        if input.rate() > 0.0 {
            self.rate = input.rate();
        }
        let n = input.samples();
        if n == 0 || input.is_empty() {
            return;
        }
        let k = n.min(self.size);
        self.ring.copy_within(k.., 0);
        let start = self.size - k;
        let channels = input.len() as f32;
        for (i, slot) in self.ring[start..].iter_mut().enumerate() {
            let s = n - k + i;
            *slot = (0..input.len()).map(|c| input.channel(c)[s]).sum::<f32>() / channels;
        }
    }

    /// RMS of the current window.
    pub fn rms(&self) -> f32 {
        (self.ring.iter().map(|s| s * s).sum::<f32>() / self.size.max(1) as f32).sqrt()
    }

    /// Windowed FFT of the current window; bins `0..size/2` are meaningful.
    pub fn analyse(&mut self) -> &[Complex32] {
        for (b, (s, w)) in self.buf.iter_mut().zip(self.ring.iter().zip(&self.window)) {
            *b = Complex32::new(s * w, 0.0);
        }
        let fft = self.fft.as_ref().expect("prepare() first");
        fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        &self.buf
    }

    /// The input sample rate, or 48 kHz before any audio arrived.
    pub fn rate_or_default(&self) -> f32 {
        if self.rate > 0.0 { self.rate } else { 48_000.0 }
    }
}

/// Band energies (log-spaced) and RMS level of an audio-rate input.
#[derive(Default)]
pub struct Spectrum {
    win: FftWindow,
    smoothed: Vec<f32>,
}

impl SigOp for Spectrum {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let size: usize = p.str("fft_size").parse().unwrap_or(1024);
        self.win.prepare(size);
        if let Some(input) = ctx.inputs.get(0) {
            self.win.push(input);
        }
        let bands = (p.i64("bands").clamp(1, 32)) as usize;
        self.smoothed.resize(bands, 0.0);

        let rms = self.win.rms();
        let rate = self.win.rate_or_default();
        let nyquist = rate / 2.0;
        let lo = p.f32("min_freq").clamp(1.0, nyquist);
        let hi = p.f32("max_freq").clamp(lo * 1.001, nyquist);
        let bin_hz = rate / size as f32;
        // A full-scale sine reads ~1: single-sided (×2) over the Hann gain (×2).
        let norm = 4.0 / size as f32;
        let smoothing = p.f32("smoothing").clamp(0.0, 0.99);
        let gain = p.f32("gain");
        let spectrum = self.win.analyse();
        let mut w = ctx.output.writer();
        for (b, name) in BAND_NAMES.iter().enumerate().take(bands) {
            let f0 = lo * (hi / lo).powf(b as f32 / bands as f32);
            let f1 = lo * (hi / lo).powf((b + 1) as f32 / bands as f32);
            let i0 = ((f0 / bin_hz).floor() as usize).max(1);
            let i1 = ((f1 / bin_hz).ceil() as usize).clamp(i0 + 1, size / 2);
            let peak = spectrum[i0..i1]
                .iter()
                .map(|c| c.norm())
                .fold(0.0, f32::max)
                * norm;
            let s = &mut self.smoothed[b];
            *s = *s * smoothing + peak * (1.0 - smoothing);
            w.push(name, *s * gain);
        }
        w.push("level", rms * gain);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use atd_core::graph::{Edit, Network, Node, NodeId, Param};
    use atd_core::registry::Registry;
    use atd_gpu::engine::{Params, SigInputs};
    use serde_json::json;

    use super::*;

    fn net_with(ty: &str, params: &[(&str, serde_json::Value)]) -> (Registry, Network, NodeId) {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, ty, "n1"),
            },
        )
        .unwrap();
        for (p, v) in params {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: (*p).into(),
                    value: Some(Param::constant(v.clone())),
                },
            )
            .unwrap();
        }
        (reg, net, id)
    }

    fn sine(freq: f32, rate: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / rate).sin())
            .collect()
    }

    #[test]
    fn audio_file_plays_wav_in_blocks_without_blocking() {
        let dir = std::env::temp_dir().join(format!("atd-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wr = hound::WavWriter::create(&path, spec).unwrap();
        let tone = sine(440.0, 48_000.0, 48_000, 0.5);
        for s in &tone {
            let v = (s * 32767.0) as i16;
            wr.write_sample(v).unwrap();
            wr.write_sample(-v).unwrap();
        }
        wr.finalize().unwrap();

        let (reg, net, id) = net_with(
            "sig.audio_file_in",
            &[("file", json!(path.to_string_lossy()))],
        );
        let sig = HashMap::new();
        let mut op = AudioFileIn::default();
        let mut out = Channels::default();
        let cook = |op: &mut AudioFileIn, out: &mut Channels, dt: f64| {
            let mut ctx = SigCtx {
                inputs: SigInputs::new(&[], &sig),
                output: out,
                params: Params::new(&net, &reg, &sig, id),
                time: 0.0,
                dt,
                frame: 0,
            };
            op.cook(&mut ctx)
        };
        // The first cook returns at once while decoding continues elsewhere.
        cook(&mut op, &mut out, 0.0).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while op.clip.is_none() {
            assert!(std::time::Instant::now() < deadline, "decoding finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
            cook(&mut op, &mut out, 0.0).unwrap();
        }
        cook(&mut op, &mut out, 0.01).unwrap();
        assert_eq!(out.names().collect::<Vec<_>>(), ["left", "right"]);
        assert_eq!((out.samples(), out.rate()), (480, 48_000.0));
        for i in [10, 100, 479] {
            assert!((out.channel(0)[i] - tone[i]).abs() < 1e-3);
            assert!((out.channel(1)[i] + tone[i]).abs() < 1e-3);
        }
        // The next block continues where the last ended.
        cook(&mut op, &mut out, 0.01).unwrap();
        assert!((out.channel(0)[0] - tone[480]).abs() < 1e-3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn audio_file_reports_missing_files() {
        let (reg, net, id) = net_with(
            "sig.audio_file_in",
            &[("file", json!("/nonexistent/x.wav"))],
        );
        let sig = HashMap::new();
        let mut op = AudioFileIn::default();
        let mut out = Channels::default();
        let mut result = Ok(());
        for _ in 0..200 {
            let mut ctx = SigCtx {
                inputs: SigInputs::new(&[], &sig),
                output: &mut out,
                params: Params::new(&net, &reg, &sig, id),
                time: 0.0,
                dt: 0.01,
                frame: 0,
            };
            result = op.cook(&mut ctx);
            if result.is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(result.unwrap_err().contains("x.wav"));
    }

    #[test]
    fn spectrum_finds_a_tone_in_its_band() {
        let (reg, mut net, id) = net_with(
            "sig.spectrum",
            &[("smoothing", json!(0.0)), ("bands", json!(8))],
        );
        let src = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(src, "sig.constant", "src"),
            },
        )
        .unwrap();
        let mut sig = HashMap::new();
        let mut block = Channels::default();
        block.set_block(&["mono"], 48_000.0, 1024);
        block
            .channel_mut(0)
            .copy_from_slice(&sine(1000.0, 48_000.0, 1024, 1.0));
        sig.insert(src, block);
        let mut op = Spectrum::default();
        let mut out = Channels::default();
        let sources = [Some(src)];
        let mut ctx = SigCtx {
            inputs: SigInputs::new(&sources, &sig),
            output: &mut out,
            params: Params::new(&net, &reg, &sig, id),
            time: 0.0,
            dt: 1024.0 / 48_000.0,
            frame: 0,
        };
        op.cook(&mut ctx).unwrap();
        assert_eq!(out.len(), 9, "8 bands + level");
        let bands: Vec<f32> = (0..8).map(|b| out.last(b)).collect();
        let (best, peak) = bands
            .iter()
            .enumerate()
            .fold((0, 0.0), |a, (i, v)| if *v > a.1 { (i, *v) } else { a });
        // 30 Hz–16 kHz in 8 log bands: 1 kHz falls in band 5 (index 4).
        assert_eq!(best, 4, "bands: {bands:?}");
        assert!(peak > 0.7 && peak < 1.3, "full-scale sine reads ~1: {peak}");
        assert!(
            bands
                .iter()
                .enumerate()
                .all(|(i, v)| i == 4 || *v < peak * 0.25),
            "{bands:?}"
        );
        let level = out.value("level").unwrap();
        assert!(
            (level - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02,
            "sine RMS: {level}"
        );
    }
}
