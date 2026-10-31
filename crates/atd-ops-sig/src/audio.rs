//! Audio operators: live input, file playback and spectrum analysis.
//!
//! Audio arrives as audio-rate [`Channels`] blocks: each engine frame carries
//! the samples played since the previous frame. Decoding happens on a worker
//! thread, never on the cook path (CLAUDE.md rule 1).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_gpu::engine::{Channels, SigCtx, SigOp};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Most samples emitted in one frame (protects against long stalls).
pub(crate) const MAX_BLOCK: usize = 16_384;

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
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_or(u64::MAX, |m| m.len());
    decode_wav_from(std::io::BufReader::new(file), len)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Decodes WAV bytes from `source`, at most `len` bytes long. The sample
/// count in the header is not trusted for allocation: a damaged header can
/// claim billions of samples.
pub fn decode_wav_from(source: impl std::io::Read, len: u64) -> Result<Clip, String> {
    let mut reader = hound::WavReader::new(source).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.sample_format == hound::SampleFormat::Int && !(1..=32).contains(&spec.bits_per_sample) {
        return Err(format!(
            "{}-bit samples are not supported",
            spec.bits_per_sample
        ));
    }
    let n = usize::from(spec.channels.max(1));
    // Every sample takes at least one byte of the file.
    let capacity = u64::from(reader.len()).min(len).min(usize::MAX as u64) as usize;
    let mut interleaved: Vec<f32> = Vec::with_capacity(capacity);
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                interleaved.push(s.map_err(|e| e.to_string())?);
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.samples::<i32>() {
                interleaved.push(s.map_err(|e| e.to_string())? as f32 * scale);
            }
        }
    }
    let mut channels = vec![Vec::with_capacity(interleaved.len() / n); n];
    for frame in interleaved.chunks_exact(n) {
        for (c, s) in frame.iter().enumerate() {
            // NaN or infinite float samples would poison everything downstream.
            channels[c].push(if s.is_finite() { *s } else { 0.0 });
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
                let file = p.resolve(path);
                std::thread::Builder::new()
                    .name("atd-audio-decode".into())
                    .spawn(move || {
                        let _ = tx.send(decode_wav(&file));
                    })
                    .map_err(|e| e.to_string())?;
                self.loading = Some(rx);
            }
        }
        // Offline the first cook waits for the decode (FR-H1).
        if let Some(rx) = &self.loading {
            match p.mode().receive(rx) {
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
        let play = p.bool("play") && frames > 0;
        let rate = f64::from(clip.rate);
        let count = match p.mode().step() {
            // Offline the playhead is a function of time alone (FR-H1):
            // each frame plays the samples from one step ago up to now.
            Some(step) if play => {
                let end = (ctx.time * rate).round().max(0.0);
                let start = ((ctx.time - step) * rate).round().max(0.0);
                self.pos = if p.bool("loop") {
                    start % frames as f64
                } else {
                    start.min(frames as f64)
                };
                ((end - start) as usize).min(MAX_BLOCK)
            }
            None if play => ((ctx.dt * rate).round() as usize).min(MAX_BLOCK),
            _ => 0,
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

/// Live audio input (device capture on an `atd-io` thread).
#[derive(Debug, Default)]
pub struct AudioIn {
    /// Device and active state the open capture was started with.
    opened: Option<(String, bool)>,
    input: Option<atd_io::audio_in::AudioIn>,
    /// Interleaved samples drained from the ring, reused.
    scratch: Vec<f32>,
}

const MULTI_NAMES: [&str; 8] = [
    "chan1", "chan2", "chan3", "chan4", "chan5", "chan6", "chan7", "chan8",
];

impl SigOp for AudioIn {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A live input is idle in an offline render (FR-H1): no device
            // is opened and the output is silence, as when inactive.
            ctx.output.set_block(&[], 0.0, 0);
            return Ok(());
        }
        let device = p.str("device");
        let active = p.bool("active");
        let changed = self
            .opened
            .as_ref()
            .is_none_or(|(d, a)| d != device || *a != active);
        if changed {
            // Opening never blocks: the device opens on its own thread.
            self.input = active.then(|| atd_io::audio_in::AudioIn::open(device));
            self.opened = Some((device.to_owned(), active));
        }
        let Some(input) = &self.input else {
            ctx.output.set_block(&[], 0.0, 0);
            return Ok(());
        };
        if let Some(e) = input.error() {
            ctx.output.set_block(&[], 0.0, 0);
            return Err(e.to_owned());
        }
        let (Some(format), Some(ring)) = (input.format(), input.ring()) else {
            ctx.output.set_block(&[], 0.0, 0);
            return Ok(());
        };
        let stride = usize::from(format.channels.max(1));
        let names: &[&str] = match stride {
            1 => &["mono"],
            2 => &["left", "right"],
            n => &MULTI_NAMES[..n.min(MULTI_NAMES.len())],
        };
        // Keep the newest MAX_BLOCK frames if the cook fell behind.
        let mut frames = ring.available() / stride;
        if frames > MAX_BLOCK {
            ring.skip((frames - MAX_BLOCK) * stride);
            frames = MAX_BLOCK;
        }
        if self.scratch.capacity() < MAX_BLOCK * stride {
            self.scratch
                .reserve(MAX_BLOCK * stride - self.scratch.len());
        }
        self.scratch.clear();
        self.scratch.resize(frames * stride, 0.0);
        let got = ring.pop(&mut self.scratch) / stride;
        ctx.output.reserve(MAX_BLOCK * names.len());
        ctx.output.set_block(names, format.rate as f32, got);
        let gain = p.f32("gain");
        for c in 0..names.len() {
            for (i, out) in ctx.output.channel_mut(c).iter_mut().enumerate() {
                *out = self.scratch[i * stride + c] * gain;
            }
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
        // Keep 1 ≤ lo < hi ≤ Nyquist whatever the parameters and the rate.
        let lo = p.f32("min_freq").clamp(1.0, (nyquist / 1.001).max(1.0));
        let hi = p.f32("max_freq").clamp(lo * 1.001, nyquist.max(lo * 1.001));
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
            let top = size / 2;
            let i0 = ((f0 / bin_hz).floor() as usize).clamp(1, top - 1);
            let i1 = ((f1 / bin_hz).ceil() as usize).clamp(i0 + 1, top);
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

    fn wav_bytes(spec: hound::WavSpec, samples: usize) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        let mut wr = hound::WavWriter::new(&mut bytes, spec).unwrap();
        for i in 0..samples {
            match spec.sample_format {
                hound::SampleFormat::Float => wr.write_sample((i as f32 * 0.1).sin()).unwrap(),
                hound::SampleFormat::Int => wr.write_sample((i * 37 % 200) as i8).unwrap(),
            }
        }
        wr.finalize().unwrap();
        bytes.into_inner()
    }

    #[test]
    fn damaged_wav_headers_and_non_finite_samples() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 8,
            sample_format: hound::SampleFormat::Int,
        };
        let mut bytes = wav_bytes(spec, 100);
        // The data chunk claims 4 GB: 4 billion samples, 16 GB as f32.
        let data = bytes.windows(4).position(|w| w == b"data").unwrap();
        bytes[data + 4..data + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        let len = bytes.len() as u64;
        // Truncated data is an error, after reading what is there.
        assert!(decode_wav_from(&bytes[..], len).is_err());
        let ok = decode_wav_from(&wav_bytes(spec, 100)[..], len).unwrap();
        assert_eq!((ok.rate, ok.channels.len(), ok.frames()), (8_000, 1, 100));

        let float = hound::WavSpec {
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
            ..spec
        };
        let mut bytes = std::io::Cursor::new(Vec::new());
        let mut wr = hound::WavWriter::new(&mut bytes, float).unwrap();
        for v in [0.5, f32::NAN, f32::INFINITY, -0.25] {
            wr.write_sample(v).unwrap();
        }
        wr.finalize().unwrap();
        let bytes = bytes.into_inner();
        let clip = decode_wav_from(&bytes[..], bytes.len() as u64).unwrap();
        assert_eq!(clip.channels[0], [0.5, 0.0, 0.0, -0.25]);
    }

    /// Damaged WAV files never panic, and decoded samples are finite.
    #[test]
    fn mutated_wavs_never_panic() {
        let mut seed = 0x6a09_e667_f3bc_c908_u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let spec = |channels, bits, sample_format| hound::WavSpec {
            channels,
            sample_rate: 44_100,
            bits_per_sample: bits,
            sample_format,
        };
        let seeds = [
            wav_bytes(spec(1, 8, hound::SampleFormat::Int), 64),
            wav_bytes(spec(2, 32, hound::SampleFormat::Float), 64),
            wav_bytes(spec(3, 8, hound::SampleFormat::Int), 63),
        ];
        let rounds: usize = std::env::var("ATD_FUZZ_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000);
        let mut decoded = 0;
        for _ in 0..rounds {
            let mut bytes = seeds[(rand() % 3) as usize].clone();
            let len = bytes.len();
            match rand() % 4 {
                0 => {
                    for _ in 0..1 + rand() % 4 {
                        let i = (rand() as usize) % len.min(48);
                        bytes[i] = rand() as u8;
                    }
                }
                1 => {
                    // A size or count field forced to an extreme.
                    let i = 4 * ((rand() as usize) % 11);
                    let v = [0u32, 1, u32::MAX, 0x8000_0000][(rand() % 4) as usize];
                    bytes[i..i + 4].copy_from_slice(&v.to_le_bytes());
                }
                2 => bytes.truncate((rand() as usize) % (len + 1)),
                _ => {
                    let i = (rand() as usize) % len;
                    bytes[i] = rand() as u8;
                }
            }
            if let Ok(clip) = decode_wav_from(&bytes[..], bytes.len() as u64) {
                decoded += 1;
                assert!(clip.channels.iter().flatten().all(|s| s.is_finite()));
            }
        }
        assert!(decoded > 0);
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
                dat: atd_gpu::engine::DatIo::none(),
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
                dat: atd_gpu::engine::DatIo::none(),
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
            dat: atd_gpu::engine::DatIo::none(),
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

    fn cook_audio_in(
        op: &mut AudioIn,
        out: &mut Channels,
        reg: &Registry,
        net: &Network,
        id: NodeId,
    ) -> Result<(), String> {
        let sig = HashMap::new();
        let mut ctx = SigCtx {
            dat: atd_gpu::engine::DatIo::none(),
            inputs: SigInputs::new(&[], &sig),
            output: out,
            params: Params::new(net, reg, &sig, id),
            time: 0.0,
            dt: 0.016,
            frame: 0,
        };
        op.cook(&mut ctx)
    }

    #[test]
    fn audio_in_streams_the_test_tone_and_releases_it() {
        let (reg, mut net, id) = net_with(
            "sig.audio_in",
            &[
                ("device", json!(atd_io::audio_in::TEST_TONE)),
                ("gain", json!(2.0)),
            ],
        );
        let mut op = AudioIn::default();
        let mut out = Channels::default();
        let start = std::time::Instant::now();
        cook_audio_in(&mut op, &mut out, &reg, &net, id).unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "never blocks"
        );
        // Collect about 0.1 s of audio.
        let mut left = Vec::new();
        while left.len() < 4800 {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "audio arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
            cook_audio_in(&mut op, &mut out, &reg, &net, id).unwrap();
            if !out.is_empty() {
                assert_eq!(out.names().collect::<Vec<_>>(), ["left", "right"]);
                assert_eq!(out.rate(), 48_000.0);
                left.extend_from_slice(out.channel(0));
            }
        }
        // Gain 2 on a 0.5 sine: peak ≈ 1; 440 Hz → about 88 zero crossings in 0.1 s.
        let peak = left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((0.9..=1.0).contains(&peak), "peak {peak}");
        let crossings = left[..4800]
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((84..=92).contains(&crossings), "{crossings} crossings");

        net.apply(
            &reg,
            Edit::SetParam {
                node: id,
                param: "active".into(),
                value: Some(Param::constant(json!(false))),
            },
        )
        .unwrap();
        cook_audio_in(&mut op, &mut out, &reg, &net, id).unwrap();
        assert!(
            out.is_empty() && op.input.is_none(),
            "inactive releases the device"
        );
    }

    #[test]
    fn audio_in_reports_unknown_devices() {
        let (reg, net, id) = net_with("sig.audio_in", &[("device", json!("no such input"))]);
        let mut op = AudioIn::default();
        let mut out = Channels::default();
        let start = std::time::Instant::now();
        loop {
            match cook_audio_in(&mut op, &mut out, &reg, &net, id) {
                Ok(()) => assert!(out.is_empty()),
                Err(e) => {
                    assert!(e.contains("no such input") || e.contains("audio"), "{e}");
                    break;
                }
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "error reported"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
