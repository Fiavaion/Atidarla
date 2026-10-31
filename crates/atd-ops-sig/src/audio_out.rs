//! Audio Out (PRD §6, SIG audio): plays audio-rate channels on an output
//! device through `atd_io::audio_out` (device thread, lock-free ring).

use atd_core::registry::ParamSchema;
use atd_gpu::engine::{SigCtx, SigOp};
use atd_io::audio_out::{MAX_LATENCY, Resampler};

use crate::Def;
use crate::audio::MAX_BLOCK;
use crate::ops::sig;

#[derive(Default)]
pub struct AudioOut {
    opened: Option<(String, bool)>,
    output: Option<atd_io::audio_out::AudioOut>,
    resampler: Resampler,
    /// Interleaved input at the source rate, then at the device rate.
    interleaved: Vec<f32>,
    resampled: Vec<f32>,
}

impl SigOp for AudioOut {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // Sinks do not emit in an offline render (FR-H1): pass through.
            match ctx.inputs.get(0) {
                Some(input) => ctx.output.copy_from(input),
                None => ctx.output.set_single(&[], &[]),
            }
            return Ok(());
        }
        let device = p.str("device");
        let active = p.bool("active");
        let changed = self
            .opened
            .as_ref()
            .is_none_or(|(d, a)| d != device || *a != active);
        if changed {
            self.output = active.then(|| atd_io::audio_out::AudioOut::open(device));
            self.opened = Some((device.to_owned(), active));
            self.resampler = Resampler::default();
        }
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        ctx.output.copy_from(input);
        let Some(output) = &self.output else {
            return Ok(());
        };
        if let Some(e) = output.error() {
            return Err(e.to_owned());
        }
        let (Some(format), Some(ring)) = (output.format(), output.ring()) else {
            return Ok(());
        };
        // Control-rate channels carry no audio.
        if input.rate() <= 0.0 || input.is_empty() || input.samples() == 0 {
            return Ok(());
        }
        let device_channels = usize::from(format.channels.max(1));
        let frames = input.samples().min(MAX_BLOCK);
        let gain = p.f32("gain");
        if self.interleaved.capacity() < MAX_BLOCK * device_channels {
            self.interleaved
                .reserve(MAX_BLOCK * device_channels - self.interleaved.len());
            // Room for upsampling up to 4× (e.g. 48 kHz from 12 kHz).
            self.resampled
                .reserve(4 * MAX_BLOCK * device_channels - self.resampled.len());
        }
        // Mono feeds every device channel; otherwise channel c → output c.
        self.interleaved.clear();
        for f in 0..frames {
            for d in 0..device_channels {
                let v = match input.len() {
                    1 => input.channel(0)[f],
                    n if d < n => input.channel(d)[f],
                    _ => 0.0,
                };
                self.interleaved.push(v * gain);
            }
        }
        self.resampled.clear();
        self.resampler.process(
            &self.interleaved,
            device_channels,
            f64::from(input.rate()),
            f64::from(format.rate),
            &mut self.resampled,
        );
        // Keep latency bounded: queue no more than MAX_LATENCY ahead.
        let cap = (MAX_LATENCY * f64::from(format.rate)) as usize * device_channels;
        let room = cap.saturating_sub(ring.available());
        let n = self.resampled.len().min(room) / device_channels * device_channels;
        ring.push(self.resampled[..n].iter().copied());
        Ok(())
    }
}

pub static DEFS6: &[Def] = &[Def {
    schema: || {
        let mut s = sig(
            "audio_out",
            "Audio Out",
            "Plays audio-rate channels (from Audio In, Audio File In or audio processing) on an output device; mono feeds every speaker, otherwise channels map in order. Converts to the device rate; passes the input through.",
            &["in"],
            vec![
                ParamSchema::string("device", "Device", "")
                    .help("Output device name; empty for the system default. `null` plays nowhere (for testing).")
                    .suggest("audio.outputs"),
                ParamSchema::toggle("active", "Active", true)
                    .help("Open the device and play; off releases it."),
                ParamSchema::float("gain", "Gain", 1.0, Some(0.0), None)
                    .help("Output level (1 = unchanged); samples are clipped to −1…1 at the device."),
            ],
            false,
        );
        s.sink = true;
        s
    },
    create: || Box::<AudioOut>::default(),
}];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use atd_core::graph::{Edit, Network, Node, Param};
    use atd_core::registry::Registry;
    use atd_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::json;

    use super::*;

    #[test]
    fn plays_mono_on_both_channels_at_the_device_rate() {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        let src = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.audio_out", "out1"),
            },
        )
        .unwrap();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(src, "sig.constant", "c1"),
            },
        )
        .unwrap();
        for (p, v) in [("device", json!("null")), ("gain", json!(0.5))] {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: p.into(),
                    value: Some(Param::constant(v)),
                },
            )
            .unwrap();
        }
        let mut op = AudioOut::default();
        let mut out = Channels::default();
        let sources = [Some(src)];
        // Constant 0.8 at 24 kHz, 240 samples per 10 ms frame.
        let mut block = Channels::default();
        block.set_block(&["mono"], 24_000.0, 240);
        block.channel_mut(0).fill(0.8);
        let mut sig = HashMap::new();
        sig.insert(src, block);
        let start = Instant::now();
        let mut cook = |op: &mut AudioOut, f: u64| {
            let mut ctx = SigCtx {
                dat: atd_gpu::engine::DatIo::none(),
                inputs: SigInputs::new(&sources, &sig),
                output: &mut out,
                params: Params::new(&net, &reg, &sig, id),
                time: f as f64 * 0.01,
                dt: 0.01,
                frame: f,
            };
            op.cook(&mut ctx).unwrap();
            out.value("mono")
        };
        // Open the device before pushing anything.
        cook(&mut op, 0);
        while op.output.as_ref().unwrap().ring().is_none() {
            assert!(start.elapsed() < Duration::from_secs(5), "device opened");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Push each 10 ms block only once the device has played the last,
        // so the latency cap never drops audio however slow the machine.
        for f in 1..=20 {
            while op.output.as_ref().unwrap().ring().unwrap().available() > 0 {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "device keeps playing"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(cook(&mut op, f), Some(0.8), "passes the input through");
        }
        while op.output.as_ref().unwrap().ring().unwrap().available() > 0 {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(50));
        let played = op.output.as_ref().unwrap().captured();
        let loud: Vec<&[f32]> = played.chunks(2).filter(|fr| fr[0] != 0.0).collect();
        // 20 × 10 ms at 48 kHz = 9600 frames, less the resampler's one-frame
        // carry at the very end.
        assert!(
            (loud.len() as i64 - 9600).abs() <= 2,
            "{} frames played",
            loud.len()
        );
        assert!(
            loud.iter()
                .all(|fr| (fr[0] - 0.4).abs() < 1e-6 && fr[0] == fr[1])
        );
    }
}
