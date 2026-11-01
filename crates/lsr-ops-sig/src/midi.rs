//! MIDI In and MIDI Out (PRD §6, SIG devices & protocols), over
//! `lsr_io::midi`. The ports run on their own threads; the cook drains and
//! queues messages without blocking (rule 1).

use std::collections::BTreeMap;

use lsr_core::registry::ParamSchema;
use lsr_gpu::engine::{SigCtx, SigOp};
use lsr_io::midi::{Message, MidiIn, MidiOut, control_change};

use crate::Def;
use crate::ops::sig;

/// Device name for a port with no hardware behind it (tests, internal
/// routing).
const VIRTUAL: &str = "virtual";

/// Kinds of channel, in output order within a MIDI channel.
const CC: u8 = 0;
const NOTE: u8 = 1;
const BEND: u8 = 2;
const PRESSURE: u8 = 3;
const PROGRAM: u8 = 4;

#[derive(Debug, Default)]
pub struct MidiInOp {
    opened: Option<String>,
    input: Option<MidiIn>,
    /// Filter the current channel names were made for (0 = omni).
    filter: i64,
    /// (MIDI channel, kind, number) → (channel name, raw value).
    controls: BTreeMap<(u8, u8, u8), (String, f32)>,
    messages: Vec<Message>,
}

impl MidiInOp {
    fn set(&mut self, channel: u8, kind: u8, number: u8, value: f32) {
        let filter = self.filter;
        self.controls
            .entry((channel, kind, number))
            .or_insert_with(|| {
                let base = match kind {
                    CC => format!("cc{number}"),
                    NOTE => format!("note{number}"),
                    BEND => "bend".to_owned(),
                    PRESSURE => "pressure".to_owned(),
                    _ => "program".to_owned(),
                };
                let name = if filter == 0 {
                    format!("ch{channel}_{base}")
                } else {
                    base
                };
                (name, 0.0)
            })
            .1 = value;
    }
}

impl SigOp for MidiInOp {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        // A live input is idle in an offline render (FR-H1): a port with no
        // device behind it, so no channels.
        let device = if p.mode().is_offline() {
            VIRTUAL
        } else {
            p.str("device")
        };
        if self.opened.as_deref() != Some(device) {
            self.input = Some(if device == VIRTUAL {
                MidiIn::detached()
            } else {
                MidiIn::open(device)
            });
            self.opened = Some(device.to_owned());
            self.controls.clear();
        }
        let filter = p.i64("channel").clamp(0, 16);
        if filter != self.filter {
            self.filter = filter;
            self.controls.clear();
        }
        let input = self.input.as_ref().expect("opened above");
        if let Some(e) = input.error() {
            ctx.output.set_single(&[], &[]);
            return Err(e.to_owned());
        }
        input.drain(&mut self.messages);
        let messages = std::mem::take(&mut self.messages);
        for msg in &messages {
            let channel = match *msg {
                Message::NoteOn { channel, .. }
                | Message::NoteOff { channel, .. }
                | Message::Control { channel, .. }
                | Message::PitchBend { channel, .. }
                | Message::ChannelPressure { channel, .. }
                | Message::Program { channel, .. } => channel,
                // Timecode is Timecode In's business, the beat clock's
                // messages Beat's.
                Message::QuarterFrame { .. }
                | Message::TimecodeFull { .. }
                | Message::Clock { .. }
                | Message::Start { .. }
                | Message::Continue { .. }
                | Message::Stop { .. }
                | Message::SongPosition { .. } => continue,
            };
            if filter != 0 && i64::from(channel) != filter {
                continue;
            }
            match *msg {
                Message::NoteOn { note, velocity, .. } => {
                    self.set(channel, NOTE, note, f32::from(velocity));
                }
                Message::NoteOff { note, .. } => self.set(channel, NOTE, note, 0.0),
                Message::Control {
                    controller, value, ..
                } => self.set(channel, CC, controller, f32::from(value)),
                Message::PitchBend { value, .. } => self.set(channel, BEND, 0, f32::from(value)),
                Message::ChannelPressure { value, .. } => {
                    self.set(channel, PRESSURE, 0, f32::from(value));
                }
                Message::Program { program, .. } => {
                    self.set(channel, PROGRAM, 0, f32::from(program));
                }
                _ => {}
            }
        }
        self.messages = messages;

        let normalise = p.bool("normalise");
        let mut w = ctx.output.writer();
        for (&(_, kind, _), (name, raw)) in &self.controls {
            let v = match (normalise, kind) {
                (false, _) => *raw,
                (true, BEND) => *raw / 8192.0,
                (true, _) => *raw / 127.0,
            };
            w.push(name, v);
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct MidiOutOp {
    opened: Option<String>,
    output: Option<MidiOut>,
    /// Last value sent per input channel.
    last: Vec<Option<u8>>,
}

impl SigOp for MidiOutOp {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        // Sinks do not emit in an offline render (FR-H1): a port with no
        // device behind it.
        let device = if p.mode().is_offline() {
            VIRTUAL
        } else {
            p.str("device")
        };
        if self.opened.as_deref() != Some(device) {
            self.output = Some(if device == VIRTUAL {
                MidiOut::detached()
            } else {
                MidiOut::open(device)
            });
            self.opened = Some(device.to_owned());
            self.last.clear();
        }
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        ctx.output.copy_from(input);
        let output = self.output.as_ref().expect("opened above");
        if let Some(e) = output.error() {
            return Err(e.to_owned());
        }
        let channel = p.i64("channel").clamp(1, 16) as u8;
        let first = p.i64("first_cc").clamp(0, 127) as usize;
        if self.last.len() < input.len() {
            self.last.resize(input.len(), None);
        }
        for c in 0..input.len().min(128 - first) {
            let v = (input.last(c).clamp(0.0, 1.0) * 127.0).round() as u8;
            if self.last[c] != Some(v) {
                output.send(control_change(channel, (first + c) as u8, v));
                self.last[c] = Some(v);
            }
        }
        Ok(())
    }
}

pub static DEFS3: &[Def] = &[
    Def {
        schema: || {
            sig(
                "midi_in",
                "MIDI In",
                "Channels from a MIDI input: controllers (`ch1_cc7`), notes (velocity while held, 0 after release), pitch bend, pressure and program, added as they arrive. The port runs on its own thread.",
                &[],
                vec![
                    ParamSchema::string("device", "Device", "")
                        .help("Part of the input port's name; empty for the first port; `virtual` for no device.")
                        .suggest("midi.inputs"),
                    ParamSchema::int("channel", "Channel", 0, Some(0), Some(16))
                        .help("MIDI channel to listen to (1–16), or 0 for all; channel names drop the `chN_` prefix when one is chosen."),
                    ParamSchema::toggle("normalise", "Normalise", true)
                        .help("Scale values to 0–1 (pitch bend to −1…1) instead of raw MIDI values."),
                ],
                true,
            )
        },
        create: || Box::<MidiInOp>::default(),
    },
    Def {
        schema: || {
            let mut s = sig(
                "midi_out",
                "MIDI Out",
                "Sends each input channel (0–1) as a MIDI control change, numbered from First CC, whenever its value changes; passes the input through.",
                &["in"],
                vec![
                    ParamSchema::string("device", "Device", "")
                        .help("Part of the output port's name; empty for the first port; `virtual` for no device.")
                        .suggest("midi.outputs"),
                    ParamSchema::int("channel", "Channel", 1, Some(1), Some(16))
                        .help("MIDI channel to send on."),
                    ParamSchema::int("first_cc", "First CC", 1, Some(0), Some(127))
                        .help("Controller number for the first input channel; the rest follow."),
                ],
                false,
            );
            s.sink = true;
            s
        },
        create: || Box::<MidiOutOp>::default(),
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lsr_core::graph::{Edit, Network, Node, NodeId, Param};
    use lsr_core::registry::Registry;
    use lsr_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::{Value, json};

    use super::*;

    fn net_with(ty: &str, params: &[(&str, Value)]) -> (Registry, Network, NodeId) {
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

    #[test]
    fn midi_in_maps_messages_to_channels() {
        let (reg, net, id) = net_with("sig.midi_in", &[]);
        let sig = HashMap::new();
        let mut op = MidiInOp {
            opened: Some(String::new()),
            input: Some(MidiIn::detached()),
            ..Default::default()
        };
        let mut out = Channels::default();
        let cook = |op: &mut MidiInOp, out: &mut Channels| {
            let mut ctx = SigCtx {
                dat: lsr_gpu::engine::DatIo::none(),
                inputs: SigInputs::new(&[], &sig),
                output: out,
                params: Params::new(&net, &reg, &sig, id),
                time: 0.0,
                dt: 0.016,
                frame: 0,
            };
            op.cook(&mut ctx).unwrap();
        };
        let input = op.input.as_ref().unwrap();
        input.push(Message::Control {
            channel: 2,
            controller: 7,
            value: 127,
        });
        input.push(Message::NoteOn {
            channel: 1,
            note: 60,
            velocity: 64,
        });
        input.push(Message::PitchBend {
            channel: 1,
            value: -8192,
        });
        cook(&mut op, &mut out);
        // Sorted by channel, then kind: ch1 note, ch1 bend, ch2 cc.
        assert_eq!(
            out.names().collect::<Vec<_>>(),
            ["ch1_note60", "ch1_bend", "ch2_cc7"]
        );
        assert!((out.value("ch1_note60").unwrap() - 64.0 / 127.0).abs() < 1e-6);
        assert_eq!(out.value("ch1_bend"), Some(-1.0));
        assert_eq!(out.value("ch2_cc7"), Some(1.0));

        // Values persist between messages; note-off returns to 0.
        op.input.as_ref().unwrap().push(Message::NoteOff {
            channel: 1,
            note: 60,
        });
        cook(&mut op, &mut out);
        assert_eq!(out.value("ch1_note60"), Some(0.0));
        assert_eq!(out.value("ch2_cc7"), Some(1.0));
    }

    #[test]
    fn midi_in_channel_filter_drops_prefix_and_other_channels() {
        let (reg, net, id) = net_with(
            "sig.midi_in",
            &[("channel", json!(2)), ("normalise", json!(false))],
        );
        let sig = HashMap::new();
        let mut op = MidiInOp {
            opened: Some(String::new()),
            input: Some(MidiIn::detached()),
            ..Default::default()
        };
        let input = op.input.as_ref().unwrap();
        input.push(Message::Control {
            channel: 2,
            controller: 1,
            value: 42,
        });
        input.push(Message::Control {
            channel: 3,
            controller: 1,
            value: 99,
        });
        let mut out = Channels::default();
        let mut ctx = SigCtx {
            dat: lsr_gpu::engine::DatIo::none(),
            inputs: SigInputs::new(&[], &sig),
            output: &mut out,
            params: Params::new(&net, &reg, &sig, id),
            time: 0.0,
            dt: 0.016,
            frame: 0,
        };
        op.cook(&mut ctx).unwrap();
        assert_eq!(out.names().collect::<Vec<_>>(), ["cc1"]);
        assert_eq!(out.value("cc1"), Some(42.0));
    }

    #[test]
    fn midi_out_sends_changes_only() {
        let (reg, mut net, id) = net_with(
            "sig.midi_out",
            &[("channel", json!(3)), ("first_cc", json!(20))],
        );
        let src = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(src, "sig.constant", "src"),
            },
        )
        .unwrap();
        let mut op = MidiOutOp {
            opened: Some(String::new()),
            output: Some(MidiOut::detached()),
            ..Default::default()
        };
        let mut out = Channels::default();
        let sources = [Some(src)];
        let mut run = |values: &[f32], op: &mut MidiOutOp| {
            let mut sig = HashMap::new();
            sig.insert(src, Channels::single(&["a", "b"], values));
            let mut ctx = SigCtx {
                dat: lsr_gpu::engine::DatIo::none(),
                inputs: SigInputs::new(&sources, &sig),
                output: &mut out,
                params: Params::new(&net, &reg, &sig, id),
                time: 0.0,
                dt: 0.016,
                frame: 0,
            };
            op.cook(&mut ctx).unwrap();
            op.output.as_ref().unwrap().sent()
        };
        assert_eq!(run(&[0.0, 1.0], &mut op), [[0xB2, 20, 0], [0xB2, 21, 127]]);
        assert!(
            run(&[0.0, 1.0], &mut op).is_empty(),
            "unchanged values are not resent"
        );
        assert_eq!(
            run(&[0.5, 2.0], &mut op),
            [[0xB2, 20, 64]],
            "clamped to 0–1"
        );
    }
}
