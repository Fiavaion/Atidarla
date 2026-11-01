//! Timecode In (PRD §6, SIG devices & protocols): SMPTE position from MIDI
//! Time Code or from LTC on an audio input, via `lsr_io::timecode`.
//! Between updates the position runs on with engine time (freewheel), so
//! `time` moves smoothly at the frame rate of the output, not of the code.

use lsr_core::registry::ParamSchema;
use lsr_gpu::engine::{SigCtx, SigOp};
use lsr_io::midi::{Message, MidiIn};
use lsr_io::timecode::{LtcDecoder, MtcDecoder, Timecode, mtc_rate};

use crate::Def;
use crate::ops::sig;

const SOURCES: &[&str] = &["mtc", "ltc"];
/// Seconds without fresh code before the position is reported unlocked.
const LOCK_TIMEOUT: f64 = 0.5;

#[derive(Default)]
pub struct TimecodeIn {
    /// (source, device) the input was opened with.
    opened: Option<(String, String)>,
    midi: Option<MidiIn>,
    audio: Option<lsr_io::audio_in::AudioIn>,
    mtc: MtcDecoder,
    ltc: Option<LtcDecoder>,
    messages: Vec<Message>,
    scratch: Vec<f32>,
    mono: Vec<f32>,
    /// Last decoded position and the engine time it arrived.
    last: Option<(Timecode, f64)>,
}

impl TimecodeIn {
    fn received(&mut self, tc: Timecode, now: f64) {
        self.last = Some((tc, now));
    }
}

impl SigOp for TimecodeIn {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        // A live input is idle in an offline render (FR-H1): a MIDI port
        // with no device behind it, so no code arrives (all zero, unlocked).
        let offline = p.mode().is_offline();
        let source = if offline { "mtc" } else { p.str("source") };
        let device = if offline {
            "virtual"
        } else if source == "ltc" {
            p.str("audio_device")
        } else {
            p.str("midi_device")
        };
        let same = self
            .opened
            .as_ref()
            .is_some_and(|(s, d)| s == source && d == device);
        if !same {
            self.midi = None;
            self.audio = None;
            self.ltc = None;
            self.last = None;
            self.mtc = MtcDecoder::default();
            if source == "ltc" {
                self.audio = Some(lsr_io::audio_in::AudioIn::open(device));
            } else {
                self.midi = Some(if device == "virtual" {
                    MidiIn::detached()
                } else {
                    MidiIn::open(device)
                });
            }
            self.opened = Some((source.to_owned(), device.to_owned()));
            self.scratch.reserve(16_384 * 2);
            self.mono.reserve(16_384);
        }
        let now = ctx.time;

        if let Some(midi) = &self.midi {
            if let Some(e) = midi.error() {
                ctx.output.set_single(&[], &[]);
                return Err(e.to_owned());
            }
            midi.drain(&mut self.messages);
            let messages = std::mem::take(&mut self.messages);
            for msg in &messages {
                match *msg {
                    Message::QuarterFrame { piece, value } => {
                        if let Some(tc) = self.mtc.quarter_frame(piece, value) {
                            self.received(tc, now);
                        }
                    }
                    Message::TimecodeFull {
                        hours,
                        minutes,
                        seconds,
                        frames,
                        rate,
                    } => {
                        let tc = Timecode {
                            hours,
                            minutes,
                            seconds,
                            frames,
                            fps: mtc_rate(rate),
                        };
                        self.received(tc, now);
                    }
                    _ => {}
                }
            }
            self.messages = messages;
        }

        if let Some(audio) = &self.audio {
            if let Some(e) = audio.error() {
                ctx.output.set_single(&[], &[]);
                return Err(e.to_owned());
            }
            if let (Some(format), Some(ring)) = (audio.format(), audio.ring()) {
                let stride = usize::from(format.channels.max(1));
                let channel = (p.i64("audio_channel").max(1) as usize - 1).min(stride - 1);
                let rate = format.rate as f32;
                let decoder = self.ltc.get_or_insert_with(|| LtcDecoder::new(rate));
                // Drain in chunks that fit the reserved scratch.
                loop {
                    let frames = (ring.available() / stride).min(16_384);
                    if frames == 0 {
                        break;
                    }
                    self.scratch.clear();
                    self.scratch.resize(frames * stride, 0.0);
                    let got = ring.pop(&mut self.scratch) / stride;
                    self.mono.clear();
                    self.mono.extend(
                        self.scratch
                            .chunks_exact(stride)
                            .take(got)
                            .map(|f| f[channel]),
                    );
                    if let Some(tc) = decoder.process(&self.mono, rate) {
                        self.last = Some((tc, now));
                    }
                }
            }
        }

        let offset = f64::from(p.f32("offset"));
        let (tc, locked, time) = match self.last {
            Some((tc, at)) => {
                let since = now - at;
                let locked = since <= LOCK_TIMEOUT;
                // Freewheel while locked; hold the last position otherwise.
                let run = if locked { since.max(0.0) } else { 0.0 };
                (Some(tc), locked, tc.seconds() + run + offset)
            }
            None => (None, false, 0.0),
        };
        let (h, m, s, f, fps) = tc.map_or((0.0, 0.0, 0.0, 0.0, 0.0), |t| {
            (
                f32::from(t.hours),
                f32::from(t.minutes),
                f32::from(t.seconds),
                f32::from(t.frames),
                t.fps,
            )
        });
        ctx.output.set_single(
            &[
                "time", "hours", "minutes", "seconds", "frames", "fps", "locked",
            ],
            &[time as f32, h, m, s, f, fps, f32::from(u8::from(locked))],
        );
        Ok(())
    }
}

pub static DEFS7: &[Def] = &[Def {
    schema: || {
        sig(
            "timecode_in",
            "Timecode In",
            "SMPTE timecode from MIDI Time Code or from LTC audio. `time` is the position in seconds and keeps moving between updates; `locked` is 1 while code is arriving. The input runs on its own thread.",
            &[],
            vec![
                ParamSchema::menu("source", "Source", SOURCES, "mtc")
                    .help("`mtc` (MIDI Time Code from a MIDI port) or `ltc` (Linear Timecode on an audio input)."),
                ParamSchema::string("midi_device", "MIDI Device", "")
                    .help("MTC: part of the MIDI input's name; empty for the first port; `virtual` for no device.")
                    .suggest("midi.inputs"),
                ParamSchema::string("audio_device", "Audio Device", "")
                    .help("LTC: audio input name; empty for the system default.")
                    .suggest("audio.inputs"),
                ParamSchema::int("audio_channel", "Audio Channel", 1, Some(1), Some(32))
                    .help("LTC: which input channel carries the code."),
                ParamSchema::float("offset", "Offset", 0.0, None, None)
                    .help("Seconds added to `time`."),
            ],
            true,
        )
    },
    create: || Box::<TimecodeIn>::default(),
}];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lsr_core::graph::{Edit, Network, Node, Param};
    use lsr_core::registry::Registry;
    use lsr_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::json;

    use super::*;

    #[test]
    fn mtc_locks_freewheels_and_times_out() {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.timecode_in", "tc1"),
            },
        )
        .unwrap();
        net.apply(
            &reg,
            Edit::SetParam {
                node: id,
                param: "midi_device".into(),
                value: Some(Param::constant(json!("virtual"))),
            },
        )
        .unwrap();
        let sig = HashMap::new();
        let mut op = TimecodeIn::default();
        let mut out = Channels::default();
        let cook = |op: &mut TimecodeIn, out: &mut Channels, t: f64| {
            let mut ctx = SigCtx {
                dat: lsr_gpu::engine::DatIo::none(),
                inputs: SigInputs::new(&[], &sig),
                output: out,
                params: Params::new(&net, &reg, &sig, id),
                time: t,
                dt: 0.01,
                frame: 0,
            };
            op.cook(&mut ctx).unwrap();
        };
        cook(&mut op, &mut out, 0.0);
        assert_eq!(out.value("locked"), Some(0.0));
        // Full frame 00:01:00:00 at 30 fps arrives at t = 1.
        op.midi.as_ref().unwrap().push(Message::TimecodeFull {
            hours: 0,
            minutes: 1,
            seconds: 0,
            frames: 0,
            rate: 3,
        });
        cook(&mut op, &mut out, 1.0);
        assert_eq!(out.value("locked"), Some(1.0));
        assert_eq!(out.value("minutes"), Some(1.0));
        assert_eq!(out.value("fps"), Some(30.0));
        assert!((out.value("time").unwrap() - 60.0).abs() < 1e-4);
        // Freewheels with engine time while locked…
        cook(&mut op, &mut out, 1.25);
        assert!((out.value("time").unwrap() - 60.25).abs() < 1e-4);
        // …and holds once code stops for longer than the timeout.
        cook(&mut op, &mut out, 3.0);
        assert_eq!(out.value("locked"), Some(0.0));
        assert!((out.value("time").unwrap() - 60.0).abs() < 1e-4);
        // Quarter frames decode too (01:02:03:04 at 25 fps, plus 2 frames).
        for (i, v) in [4u8, 0, 3, 0, 2, 0, 1, 1 << 1].iter().enumerate() {
            op.midi.as_ref().unwrap().push(Message::QuarterFrame {
                piece: i as u8,
                value: *v,
            });
        }
        cook(&mut op, &mut out, 4.0);
        assert_eq!(out.value("frames"), Some(6.0));
        assert_eq!(out.value("hours"), Some(1.0));
        let want = 3600.0 + 120.0 + 3.0 + 6.0 / 25.0;
        assert!((out.value("time").unwrap() - want).abs() < 1e-3);
    }
}
