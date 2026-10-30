//! The beat clock (`sig.beat`, PRD FR-S4): an internal tempo with tap,
//! nudge and resync, or a follower of an external MIDI clock or of audio
//! onsets. Its channels (`beat`, `beat_phase`, `bar`, `bar_phase`, `bpm`,
//! `downbeat`, `beat_pulse`) are what the Stage's quantised launch and the
//! synced LFO and Timer read.
//!
//! MIDI I/O stays on its own threads (rule 1): the cook drains a queue with
//! `try_lock` and extrapolates the position to the frame's time.

use atd_core::registry::ParamSchema;
use atd_gpu::engine::{SigCtx, SigOp};
use atd_io::midi::{Message, MidiIn, MidiOut};
use atd_io::midi_clock::ClockFollower;
use atd_io::midi_clock_out::ClockOut;

use crate::Def;
use crate::ops::sig;

/// Device name for a port with no hardware behind it (tests, offline).
const VIRTUAL: &str = "virtual";

const SOURCES: &[&str] = &["internal", "midi", "audio"];

/// A gap this long (seconds) between taps starts a new tap tempo.
const TAP_RESET: f64 = 2.0;
/// Taps the tempo is averaged over.
const MAX_TAPS: usize = 8;

/// Onsets kept for tempo estimation.
const ONSETS: usize = 16;
/// Pairs further apart than this (seconds) say nothing about the tempo.
const MAX_SPAN: f64 = 4.0;
/// Histogram resolution: bins per BPM.
const BINS_PER_BPM: f64 = 2.0;
const BINS: usize = 512;

/// Tempo and phase from a stream of onsets (`sig.onset`'s output), over
/// fixed arrays so cooks allocate nothing.
#[derive(Debug, Clone)]
struct AudioFollower {
    times: [f64; ONSETS],
    count: usize,
    head: usize,
    /// Smoothed tempo; 0 until the first estimate.
    bpm: f64,
    /// Onsets accepted since the phase was last snapped.
    locked: u32,
    rejected: u32,
}

impl Default for AudioFollower {
    fn default() -> Self {
        Self {
            times: [0.0; ONSETS],
            count: 0,
            head: 0,
            bpm: 0.0,
            locked: 0,
            rejected: 0,
        }
    }
}

impl AudioFollower {
    /// Takes an onset at `time`; folds every pair of recent onsets into
    /// the tempo range as candidate tempos (an interval of `d` seconds
    /// holds `k` beats for several `k`), votes them into a histogram and
    /// moves the smoothed tempo to the peak.
    fn onset(&mut self, time: f64, min_bpm: f64, max_bpm: f64, smoothing: f64) {
        if self.count > 0 && time < self.times[(self.head + ONSETS - 1) % ONSETS] {
            self.count = 0; // time went backwards: start over
        }
        self.times[self.head] = time;
        self.head = (self.head + 1) % ONSETS;
        self.count = (self.count + 1).min(ONSETS);
        if self.count < 4 {
            return;
        }
        let (lo, hi) = (
            min_bpm.min(max_bpm).max(20.0),
            max_bpm.max(min_bpm).min(255.0),
        );
        let mut bins = [0.0f32; BINS];
        let newest = self.count - 1;
        let at = |i: usize| self.times[(self.head + ONSETS - self.count + i) % ONSETS];
        for i in 0..self.count {
            for j in (i + 1)..=newest.min(self.count - 1) {
                let d = at(j) - at(i);
                if !(0.05..=MAX_SPAN).contains(&d) {
                    continue;
                }
                // Newer intervals weigh more.
                let w = 1.0 + (j as f32 / self.count as f32);
                for k in 1..=8u32 {
                    let bpm = 60.0 * f64::from(k) / d;
                    if bpm < lo || bpm > hi {
                        continue;
                    }
                    let c = (bpm * BINS_PER_BPM) as isize;
                    for off in -3isize..=3 {
                        let b = c + off;
                        if (0..BINS as isize).contains(&b) {
                            bins[b as usize] += w * (4 - off.unsigned_abs()) as f32;
                        }
                    }
                }
            }
        }
        let Some((peak, _)) = bins
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .filter(|(_, v)| **v > 0.0)
        else {
            return;
        };
        let centre = peak as f64 / BINS_PER_BPM;
        // Refine: the weighted mean of the votes within 3 % of the peak.
        let (mut sum, mut weight) = (0.0, 0.0);
        for i in 0..self.count {
            for j in (i + 1)..self.count {
                let d = at(j) - at(i);
                if !(0.05..=MAX_SPAN).contains(&d) {
                    continue;
                }
                for k in 1..=8u32 {
                    let bpm = 60.0 * f64::from(k) / d;
                    if (bpm - centre).abs() <= centre * 0.03 {
                        sum += bpm * d;
                        weight += d; // longer spans pin the tempo harder
                    }
                }
            }
        }
        let estimate = if weight > 0.0 { sum / weight } else { centre };
        self.bpm = if self.bpm <= 0.0 {
            estimate
        } else {
            smoothing * self.bpm + (1.0 - smoothing) * estimate
        };
    }

    /// Where an onset at `beats` leaves the clock: snapped to the nearest
    /// beat while acquiring, pulled towards it once locked (an off-beat
    /// hit is ignored).
    fn align(&mut self, beats: f64, smoothing: f64) -> f64 {
        let x = beats - beats.round();
        if self.locked < 3 {
            self.locked += 1;
            return beats - x;
        }
        if x.abs() > 0.3 {
            self.rejected += 1;
            if self.rejected >= 4 {
                self.locked = 0;
                self.rejected = 0;
            }
            return beats;
        }
        self.rejected = 0;
        beats - x * (1.0 - smoothing)
    }
}

/// MIDI clock out: the port and the thread that times the pulses on it.
struct ClockLink {
    // Dropped first: its Stop goes out before the port closes.
    clock: ClockOut,
    port: MidiOut,
    device: String,
    running: bool,
    /// Stopped by Reset: the next start is from 0, not from where the
    /// clock has got to by then.
    rewind: bool,
    /// Last tempo given to the thread (it is only told when it changes).
    bpm: f64,
}

/// The `sig.beat` operator.
pub(crate) struct Beat {
    /// Position in beats for the internal and audio sources.
    beats: f64,
    prev_resync: bool,
    prev_tap: bool,
    taps: [f64; MAX_TAPS],
    tap_count: usize,
    /// Tempo from tapping; replaced when the `bpm` parameter changes.
    tap_bpm: Option<f64>,
    seen_bpm: f64,
    prev_beat: i64,
    prev_bar: i64,
    audio: AudioFollower,
    // MIDI clock in.
    opened: Option<String>,
    midi: Option<MidiIn>,
    messages: Vec<Message>,
    follower: ClockFollower,
    seen_jumps: u32,
    /// Raw follower position at the last resync (the display's zero).
    midi_offset: f64,
    out: Option<ClockLink>,
    #[cfg(test)]
    now_micros: Option<u64>,
}

impl Default for Beat {
    fn default() -> Self {
        Self {
            beats: 0.0,
            prev_resync: false,
            prev_tap: false,
            taps: [0.0; MAX_TAPS],
            tap_count: 0,
            tap_bpm: None,
            seen_bpm: -1.0,
            prev_beat: i64::MIN,
            prev_bar: i64::MIN,
            audio: AudioFollower::default(),
            opened: None,
            midi: None,
            messages: Vec::new(),
            follower: ClockFollower::new(),
            seen_jumps: 0,
            midi_offset: 0.0,
            out: None,
            #[cfg(test)]
            now_micros: None,
        }
    }
}

impl Beat {
    /// A tap at `time`; the tempo is the mean of the intervals between the
    /// last taps.
    fn tap(&mut self, time: f64) {
        if self.tap_count > 0 && time - self.taps[self.tap_count - 1] > TAP_RESET {
            self.tap_count = 0;
        }
        if self.tap_count == MAX_TAPS {
            self.taps.copy_within(1.., 0);
            self.tap_count -= 1;
        }
        self.taps[self.tap_count] = time;
        self.tap_count += 1;
        if self.tap_count >= 2 {
            let span = self.taps[self.tap_count - 1] - self.taps[0];
            if span > 0.0 {
                let bpm = 60.0 * (self.tap_count - 1) as f64 / span;
                self.tap_bpm = Some(bpm.clamp(1.0, 999.0));
            }
        }
    }

    fn now_micros(&self) -> u64 {
        #[cfg(test)]
        if let Some(t) = self.now_micros {
            return t;
        }
        atd_io::midi::now_micros()
    }
}

impl SigOp for Beat {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let per_bar = p.i64("beats_per_bar").max(1);
        let source = p.str("source");
        let reset = p.bool("reset");
        let tap = p.bool("tap");
        let resync_now = p.bool("resync") && !self.prev_resync;
        self.prev_resync = p.bool("resync");

        // Tempo: the parameter, until a tap replaces it; a changed
        // parameter takes over again.
        let param_bpm = f64::from(p.f32("bpm")).clamp(0.0, 999.0);
        if (param_bpm - self.seen_bpm).abs() > 1e-9 {
            self.seen_bpm = param_bpm;
            self.tap_bpm = None;
        }
        if tap && !self.prev_tap {
            self.tap(ctx.time);
        }
        self.prev_tap = tap;
        let nudge =
            f64::from(p.f32("nudge").clamp(-1.0, 1.0)) * f64::from(p.f32("nudge_amount")) / 100.0;
        let set_bpm = self.tap_bpm.unwrap_or(param_bpm);

        let mut jumped = resync_now;
        let (beats, bpm) = match source {
            "midi" => {
                let device = if p.mode().is_offline() {
                    VIRTUAL
                } else {
                    p.str("device")
                };
                if self.opened.as_deref() != Some(device) || self.midi.is_none() {
                    self.midi = Some(if device == VIRTUAL {
                        MidiIn::detached()
                    } else {
                        MidiIn::open(device)
                    });
                    self.opened = Some(device.to_owned());
                    self.follower = ClockFollower::new();
                    self.seen_jumps = 0;
                    self.midi_offset = 0.0;
                }
                let input = self.midi.as_ref().expect("opened above");
                if let Some(e) = input.error() {
                    ctx.output.set_single(&[], &[]);
                    return Err(e.to_owned());
                }
                input.drain(&mut self.messages);
                for msg in &self.messages {
                    self.follower.handle(msg);
                }
                let now = self.now_micros();
                let raw = self.follower.position_beats(now);
                if self.follower.jumps() != self.seen_jumps {
                    self.seen_jumps = self.follower.jumps();
                    self.midi_offset = 0.0;
                    jumped = true;
                }
                if reset || resync_now {
                    self.midi_offset = raw;
                }
                (
                    raw - self.midi_offset,
                    self.follower.bpm().unwrap_or(set_bpm),
                )
            }
            _ => {
                // Leaving MIDI releases the port.
                if self.midi.take().is_some() {
                    self.opened = None;
                }
                let audio = source == "audio";
                let mut bpm = set_bpm;
                if audio {
                    if self.audio.bpm > 0.0 {
                        bpm = self.audio.bpm;
                    }
                } else {
                    bpm *= 1.0 + nudge;
                }
                if reset || resync_now {
                    self.beats = 0.0;
                    if resync_now {
                        // The first frame of a bar; the audio follower
                        // re-acquires the phase from the next onsets.
                        self.audio.locked = 0;
                    }
                } else {
                    // Integrating keeps the phase continuous when the tempo
                    // changes.
                    self.beats += ctx.dt * bpm / 60.0;
                }
                if audio {
                    let onset = ctx.inputs.get(0).is_some_and(|c| {
                        !c.is_empty()
                            && c.samples() > 0
                            && c.value("onset").unwrap_or_else(|| c.last(0)) > 0.5
                    });
                    if onset {
                        self.audio.onset(
                            ctx.time,
                            f64::from(p.f32("min_bpm")),
                            f64::from(p.f32("max_bpm")),
                            f64::from(p.f32("smoothing")).clamp(0.0, 0.99),
                        );
                        if !reset && !resync_now {
                            self.beats = self
                                .audio
                                .align(self.beats, f64::from(p.f32("smoothing")).clamp(0.0, 0.99));
                        }
                    }
                }
                (self.beats, bpm)
            }
        };

        // MIDI clock out. Not while following a MIDI clock (that would echo
        // it back) and not in an offline render (sinks do not emit).
        if p.bool("send_clock") && source != "midi" && !p.mode().is_offline() {
            let device = p.str("out_device");
            if self.out.as_ref().is_none_or(|l| l.device != device) {
                self.out = None; // stops and closes the old port first
                let port = if device == VIRTUAL {
                    MidiOut::detached()
                } else {
                    MidiOut::open(device)
                };
                self.out = Some(ClockLink {
                    clock: ClockOut::new(port.handle()),
                    port,
                    device: device.to_owned(),
                    running: false,
                    rewind: false,
                    bpm: -1.0,
                });
            }
            let link = self.out.as_mut().expect("opened above");
            if let Some(e) = link.port.error() {
                ctx.output.set_single(&[], &[]);
                return Err(e.to_owned());
            }
            if (bpm - link.bpm).abs() > 1e-6 {
                link.clock.set_tempo(bpm);
                link.bpm = bpm;
            }
            let running = !reset && bpm > 0.0;
            if running && (resync_now || !link.running) {
                // A resync is a new beginning at 0; otherwise the transport
                // starts, or resumes, where the clock is.
                let from = if resync_now || link.rewind {
                    0.0
                } else {
                    beats
                };
                link.clock.begin(from);
                link.running = true;
                link.rewind = false;
            } else if !running && link.running {
                link.clock.stop();
                link.running = false;
                link.rewind = reset;
            }
        } else {
            self.out = None;
        }

        let per_bar_f = per_bar as f64;
        let beat = beats.floor();
        let bar = (beats / per_bar_f).floor();
        let (beat_i, bar_i) = (beat as i64, bar as i64);
        let beat_pulse = beat_i != self.prev_beat || jumped;
        let at_bar_start = beat_i.rem_euclid(per_bar) == 0;
        // A jump into the middle of a bar is not a downbeat.
        let downbeat = if jumped {
            at_bar_start
        } else {
            bar_i != self.prev_bar
        };
        self.prev_beat = beat_i;
        self.prev_bar = bar_i;
        ctx.output.set_single(
            &[
                "beat",
                "beat_phase",
                "bar",
                "bar_phase",
                "bpm",
                "downbeat",
                "beat_pulse",
            ],
            &[
                beat as f32,
                (beats - beat) as f32,
                bar as f32,
                ((beats / per_bar_f) - bar) as f32,
                bpm as f32,
                f32::from(u8::from(downbeat)),
                f32::from(u8::from(beat_pulse)),
            ],
        );
        Ok(())
    }
}

pub static DEFS9: &[Def] = &[Def {
    schema: || {
        sig(
            "beat",
            "Beat",
            "A tempo clock: `beat` count, `beat_phase` 0–1 within the beat, `bar` count and `bar_phase`, the tempo `bpm`, and one-frame pulses `downbeat` (a bar starts) and `beat_pulse` (a beat starts). Runs from its own tempo (with tap, nudge and resync), follows a MIDI clock, or follows beats in an audio onset input.",
            &["in"],
            vec![
                ParamSchema::float("bpm", "BPM", 120.0, Some(0.0), Some(999.0))
                    .help("Tempo in beats per minute; changes keep the phase continuous. Tapping replaces it until this value is changed."),
                ParamSchema::int("beats_per_bar", "Beats per Bar", 4, Some(1), Some(64))
                    .help("Beats in one bar."),
                ParamSchema::toggle("reset", "Reset", false)
                    .help("While on, holds the clock at beat zero."),
                ParamSchema::menu("source", "Source", SOURCES, "internal")
                    .help("`internal` runs at BPM. `midi` follows a MIDI clock (24 pulses per quarter note, Start, Stop, Continue, Song Position). `audio` follows beats in the input, for example the `onset` channel of an Onset operator."),
                ParamSchema::pulse("tap", "Tap")
                    .help("Tap to set the tempo: the mean interval of the last 2–8 taps; a gap over 2 seconds starts again."),
                ParamSchema::float("nudge", "Nudge", 0.0, Some(-1.0), Some(1.0))
                    .help("While away from 0, speeds the internal clock up (+) or slows it (−) by this fraction of Nudge Amount, to pull a drifting beat back in line."),
                ParamSchema::float("nudge_amount", "Nudge Amount", 6.0, Some(0.0), Some(50.0))
                    .help("Largest tempo change from Nudge, in percent."),
                ParamSchema::pulse("resync", "Resync")
                    .help("Makes the frame it fires on beat 0 of a bar (bar 0): the next downbeat is here."),
                ParamSchema::string("device", "Device", "")
                    .help("Source `midi`: part of the input port's name; empty for the first port; `virtual` for no device.")
                    .suggest("midi.inputs"),
                ParamSchema::toggle("send_clock", "Send MIDI Clock", false)
                    .help("Sends MIDI clock (24 pulses per quarter note) with Start, Continue and Stop to Out Device, from a timing thread so pulses keep their spacing at any frame rate. Not with source `midi`, and silent in offline renders."),
                ParamSchema::string("out_device", "Out Device", "")
                    .help("Part of the output port's name for the MIDI clock; empty for the first port; `virtual` for no device.")
                    .suggest("midi.outputs"),
                ParamSchema::float("min_bpm", "Min BPM", 70.0, Some(20.0), Some(255.0))
                    .help("Source `audio`: lowest tempo to report; intervals are folded into this range."),
                ParamSchema::float("max_bpm", "Max BPM", 180.0, Some(20.0), Some(255.0))
                    .help("Source `audio`: highest tempo to report."),
                ParamSchema::float("smoothing", "Smoothing", 0.7, Some(0.0), Some(0.99))
                    .help("Source `audio`: how slowly the tempo and phase follow new onsets (0 = at once)."),
            ],
            true,
        )
    },
    create: || Box::<Beat>::default(),
}];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use atd_core::graph::{Edit, Network, Node, NodeId, Param};
    use atd_core::registry::Registry;
    use atd_gpu::engine::{Channels, CookMode, Params, SigInputs};
    use serde_json::{Value, json};

    use super::*;

    pub(crate) struct Rig {
        pub reg: Registry,
        pub net: Network,
        pub sig: HashMap<NodeId, Channels>,
        pub node: NodeId,
    }

    impl Rig {
        pub fn new(params: &[(&str, Value)]) -> Self {
            let mut reg = Registry::default();
            crate::register(&mut reg);
            let mut net = Network::default();
            let node = net.alloc_id();
            net.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(node, "sig.beat", "clock"),
                },
            )
            .unwrap();
            let mut rig = Self {
                reg,
                net,
                sig: HashMap::new(),
                node,
            };
            for (k, v) in params {
                rig.set(k, v.clone());
            }
            rig
        }

        pub fn set(&mut self, name: &str, value: Value) {
            self.net
                .apply(
                    &self.reg,
                    Edit::SetParam {
                        node: self.node,
                        param: name.into(),
                        value: Some(Param::constant(value)),
                    },
                )
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }

        /// Cooks one frame; `input` is an optional upstream channel set.
        pub fn cook(
            &mut self,
            op: &mut Beat,
            input: Option<Channels>,
            time: f64,
            dt: f64,
            mode: CookMode,
        ) -> Channels {
            let up = self.net.alloc_id();
            let sources = match input {
                Some(c) => {
                    self.sig.insert(up, c);
                    vec![Some(up)]
                }
                None => vec![],
            };
            let mut out = Channels::default();
            let mut ctx = SigCtx {
                inputs: SigInputs::new(&sources, &self.sig),
                output: &mut out,
                params: Params::new(&self.net, &self.reg, &self.sig, self.node).with_mode(mode),
                time,
                dt,
                frame: 0,
            };
            op.cook(&mut ctx).unwrap();
            out
        }

        pub fn step(&mut self, op: &mut Beat, time: f64, dt: f64) -> Channels {
            self.cook(op, None, time, dt, CookMode::Live)
        }
    }

    fn v(c: &Channels, name: &str) -> f32 {
        c.value(name).unwrap_or_else(|| panic!("channel {name}"))
    }

    #[test]
    fn channels_are_exactly_the_clock_set() {
        let mut rig = Rig::new(&[]);
        let mut op = Beat::default();
        let out = rig.step(&mut op, 0.0, 0.0);
        assert_eq!(
            out.names().collect::<Vec<_>>(),
            [
                "beat",
                "beat_phase",
                "bar",
                "bar_phase",
                "bpm",
                "downbeat",
                "beat_pulse"
            ]
        );
        assert_eq!(
            (v(&out, "downbeat"), v(&out, "beat_pulse")),
            (1.0, 1.0),
            "the first frame starts a bar"
        );
        assert_eq!(v(&out, "bpm"), 120.0);
    }

    #[test]
    fn pulses_fire_on_the_frame_a_beat_or_bar_starts() {
        // 120 BPM at 0.25 s frames: a beat every 2 frames, a bar every 8.
        let mut rig = Rig::new(&[]);
        let mut op = Beat::default();
        let mut beats = Vec::new();
        let mut bars = Vec::new();
        for f in 0..17 {
            let out = rig.step(&mut op, f as f64 * 0.25, if f == 0 { 0.0 } else { 0.25 });
            beats.push(v(&out, "beat_pulse") as u8);
            bars.push(v(&out, "downbeat") as u8);
        }
        assert_eq!(beats, [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]);
        assert_eq!(bars, [1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn tap_sets_the_tempo_from_the_last_taps_and_resets_after_a_gap() {
        let mut rig = Rig::new(&[]);
        let mut op = Beat::default();
        // Taps 0.6 s apart = 100 BPM.
        let mut t = 0.0;
        let mut bpm = 0.0;
        for i in 0..5 {
            rig.set("tap", json!(true));
            bpm = v(&rig.step(&mut op, t, 0.0), "bpm");
            rig.set("tap", json!(false));
            rig.step(&mut op, t + 0.1, 0.1);
            if i == 0 {
                assert_eq!(bpm, 120.0, "one tap is not a tempo");
            }
            t += 0.6;
        }
        assert!((bpm - 100.0).abs() < 0.01, "{bpm}");
        // A change of the parameter takes over from the taps.
        rig.set("bpm", json!(90.0));
        assert_eq!(v(&rig.step(&mut op, t, 0.0), "bpm"), 90.0);
        // More than 2 s later a tap starts a new count (no tempo from one).
        t += 3.0;
        rig.set("tap", json!(true));
        assert_eq!(v(&rig.step(&mut op, t, 0.0), "bpm"), 90.0);
        rig.set("tap", json!(false));
        rig.step(&mut op, t + 0.1, 0.1);
        // Only the last 8 taps count: a slowdown after many fast taps.
        let mut op = Beat::default();
        let mut t = 0.0;
        for _ in 0..12 {
            rig.set("tap", json!(true));
            rig.step(&mut op, t, 0.0);
            rig.set("tap", json!(false));
            rig.step(&mut op, t + 0.01, 0.0);
            t += 0.25;
        }
        for _ in 0..8 {
            rig.set("tap", json!(true));
            rig.step(&mut op, t, 0.0);
            rig.set("tap", json!(false));
            rig.step(&mut op, t + 0.01, 0.0);
            t += 1.0;
        }
        rig.set("tap", json!(true));
        let bpm = v(&rig.step(&mut op, t, 0.0), "bpm");
        assert!((bpm - 60.0).abs() < 0.01, "last 8 taps only: {bpm}");
    }

    #[test]
    fn nudge_shifts_tempo_while_held() {
        let mut rig = Rig::new(&[("nudge_amount", json!(10.0))]);
        let mut op = Beat::default();
        rig.step(&mut op, 0.0, 0.0);
        rig.set("nudge", json!(1.0));
        let out = rig.step(&mut op, 1.0, 1.0);
        assert!((v(&out, "bpm") - 132.0).abs() < 1e-4);
        assert!((f64::from(v(&out, "beat")) + f64::from(v(&out, "beat_phase")) - 2.2).abs() < 1e-5);
        rig.set("nudge", json!(-1.0));
        assert!((v(&rig.step(&mut op, 2.0, 1.0), "bpm") - 108.0).abs() < 1e-4);
        rig.set("nudge", json!(0.0));
        assert_eq!(v(&rig.step(&mut op, 3.0, 1.0), "bpm"), 120.0);
    }

    #[test]
    fn resync_makes_the_next_frame_beat_zero_of_a_bar() {
        let mut rig = Rig::new(&[]);
        let mut op = Beat::default();
        for f in 0..7 {
            rig.step(&mut op, f as f64 * 0.3, if f == 0 { 0.0 } else { 0.3 });
        }
        rig.set("resync", json!(true));
        let out = rig.step(&mut op, 2.1, 0.3);
        assert_eq!(
            (v(&out, "beat"), v(&out, "beat_phase"), v(&out, "bar")),
            (0.0, 0.0, 0.0)
        );
        assert_eq!((v(&out, "downbeat"), v(&out, "beat_pulse")), (1.0, 1.0));
        // Held, it fires once; the clock runs on from there.
        let out = rig.step(&mut op, 2.4, 0.3);
        assert_eq!(v(&out, "downbeat"), 0.0);
        assert!((v(&out, "beat_phase") - 0.6).abs() < 1e-5);
    }

    #[test]
    fn an_old_projects_parameters_cook_as_before() {
        // Only the three original parameters set, no input: the first four
        // channels are the old operator's.
        let mut rig = Rig::new(&[("bpm", json!(120.0)), ("beats_per_bar", json!(4))]);
        let mut op = Beat::default();
        let mut out = Channels::default();
        for _ in 0..5 {
            out = rig.step(&mut op, 0.0, 0.25);
        }
        assert_eq!(
            (
                v(&out, "beat"),
                v(&out, "beat_phase"),
                v(&out, "bar"),
                v(&out, "bar_phase")
            ),
            (2.0, 0.5, 0.0, 0.625)
        );
    }

    /// Feeds a follower-driving op `ticks` pulses at `bpm` starting at
    /// `t0` µs, pushing them into its detached port.
    fn push_pulses(op: &Beat, bpm: f64, t0: u64, from: u32, ticks: u32) -> u64 {
        let period = 60e6 / (bpm * 24.0);
        let mut last = t0;
        for k in from..from + ticks {
            last = (t0 as f64 + f64::from(k) * period).round() as u64;
            op.midi
                .as_ref()
                .unwrap()
                .push(Message::Clock { micros: last });
        }
        last
    }

    #[test]
    fn midi_clock_source_follows_tempo_and_locks_to_start() {
        let mut rig = Rig::new(&[("source", json!("midi")), ("device", json!(VIRTUAL))]);
        let mut op = Beat {
            now_micros: Some(0),
            ..Beat::default()
        };
        let out = rig.step(&mut op, 0.0, 0.0);
        assert_eq!(v(&out, "bpm"), 120.0, "the parameter until a clock arrives");
        op.midi.as_ref().unwrap().push(Message::Start { micros: 0 });
        // Pulse 0 at 5 ms, then one beat (24 pulses) at 174 BPM.
        let last = push_pulses(&op, 174.0, 5_000, 0, 25);
        op.now_micros = Some(last);
        let out = rig.step(&mut op, 1.0, 0.016);
        assert!((v(&out, "bpm") - 174.0).abs() < 0.05, "{}", v(&out, "bpm"));
        assert_eq!(v(&out, "beat"), 1.0, "24 pulses after the first = beat 1");
        assert!(v(&out, "beat_phase") < 1e-5);
        assert_eq!((v(&out, "downbeat"), v(&out, "beat_pulse")), (0.0, 1.0));
        // Stop holds; Start rewinds to a downbeat.
        op.midi
            .as_ref()
            .unwrap()
            .push(Message::Stop { micros: last + 1 });
        op.now_micros = Some(last + 500_000);
        let held = rig.step(&mut op, 1.5, 0.5);
        assert_eq!(v(&held, "beat"), 1.0);
        op.midi.as_ref().unwrap().push(Message::Start {
            micros: last + 600_000,
        });
        op.midi.as_ref().unwrap().push(Message::Clock {
            micros: last + 601_000,
        });
        op.now_micros = Some(last + 601_000);
        let out = rig.step(&mut op, 2.0, 0.5);
        assert_eq!((v(&out, "beat"), v(&out, "bar")), (0.0, 0.0));
        assert_eq!((v(&out, "downbeat"), v(&out, "beat_pulse")), (1.0, 1.0));
    }

    #[test]
    fn midi_song_position_and_resync() {
        let mut rig = Rig::new(&[("source", json!("midi")), ("device", json!(VIRTUAL))]);
        let mut op = Beat {
            now_micros: Some(0),
            ..Beat::default()
        };
        rig.step(&mut op, 0.0, 0.0);
        // Song Position 20 sixteenths = beat 5 (bar 1, second beat).
        op.midi
            .as_ref()
            .unwrap()
            .push(Message::SongPosition { sixteenths: 20 });
        op.midi
            .as_ref()
            .unwrap()
            .push(Message::Continue { micros: 1 });
        let last = push_pulses(&op, 120.0, 1_000, 0, 1);
        op.now_micros = Some(last);
        let out = rig.step(&mut op, 0.1, 0.1);
        assert_eq!((v(&out, "beat"), v(&out, "bar")), (5.0, 1.0));
        assert_eq!(v(&out, "downbeat"), 0.0, "mid-bar");
        // Resync: the master's position here becomes beat 0 of bar 0.
        rig.set("resync", json!(true));
        let out = rig.step(&mut op, 0.2, 0.1);
        assert_eq!(
            (v(&out, "beat"), v(&out, "bar"), v(&out, "downbeat")),
            (0.0, 0.0, 1.0)
        );
    }

    #[test]
    fn midi_source_is_idle_offline() {
        let mut rig = Rig::new(&[
            ("source", json!("midi")),
            ("device", json!("some real port")),
        ]);
        let mut op = Beat::default();
        let out = rig.cook(&mut op, None, 0.0, 0.0, CookMode::Offline { fps: 60.0 });
        assert_eq!(v(&out, "beat"), 0.0);
        assert_eq!(op.opened.as_deref(), Some(VIRTUAL), "no hardware offline");
    }

    #[test]
    fn audio_source_finds_tempo_and_locks_onto_the_beat() {
        // A kick every beat at 128 BPM with a hi-hat between, 60 fps.
        let truth = 128.0;
        let period = 60.0 / truth;
        let mut rig = Rig::new(&[("source", json!("audio")), ("bpm", json!(100.0))]);
        let mut op = Beat::default();
        let dt = 1.0 / 60.0;
        let mut next_hit = 0.3;
        let mut offsets = Vec::new();
        let mut bpm = 0.0;
        for f in 0..(14 * 60) {
            let t = f as f64 * dt;
            let mut hit = 0.0;
            if t >= next_hit {
                hit = 1.0;
                next_hit += period;
            }
            let input = Channels::single(&["onset", "flux"], &[hit, 0.1]);
            let out = rig.cook(
                &mut op,
                Some(input),
                t,
                if f == 0 { 0.0 } else { dt },
                CookMode::Live,
            );
            bpm = f64::from(v(&out, "bpm"));
            if hit > 0.5 && t > 8.0 {
                let phase = f64::from(v(&out, "beat_phase"));
                offsets.push(phase.min(1.0 - phase));
            }
        }
        let worst = offsets.iter().cloned().fold(0.0, f64::max);
        println!(
            "audio follow at {truth} BPM, 60 fps: tempo {bpm:.2}, worst onset-to-beat offset {worst:.3} beats"
        );
        assert!((bpm - truth).abs() < 1.0, "{bpm}");
        assert!(worst < 0.1, "{worst}");
    }

    fn drain_out(op: &Beat) -> Vec<Vec<u8>> {
        op.out
            .as_ref()
            .expect("clock out is open")
            .port
            .sent_raw()
            .into_iter()
            .map(|(m, _)| m)
            .collect()
    }

    fn count(msgs: &[Vec<u8>], byte: u8) -> usize {
        msgs.iter().filter(|m| m.as_slice() == [byte]).count()
    }

    #[test]
    fn clock_out_pulses_come_from_the_timing_thread_not_the_frame() {
        let mut rig = Rig::new(&[("send_clock", json!(true)), ("out_device", json!(VIRTUAL))]);
        let mut op = Beat::default();
        // Two cooks only; the pulses must keep coming in between.
        rig.step(&mut op, 0.0, 0.0);
        rig.step(&mut op, 0.016, 0.016);
        std::thread::sleep(std::time::Duration::from_millis(500));
        let sent = drain_out(&op);
        assert_eq!(sent[0], [0xFA], "Start at beat 0");
        let pulses = count(&sent, 0xF8);
        assert!(
            (22..=26).contains(&pulses),
            "{pulses} pulses in 0.5 s at 120 BPM"
        );

        // A tempo change reaches the thread without another cook per pulse.
        rig.set("bpm", json!(240.0));
        rig.step(&mut op, 0.5, 0.016);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let pulses = count(&drain_out(&op), 0xF8);
        assert!(
            (20..=28).contains(&pulses),
            "{pulses} pulses in 0.25 s at 240 BPM"
        );

        // Resync is a new Start; reset stops the transport, and the pulses.
        rig.set("resync", json!(true));
        rig.step(&mut op, 0.6, 0.016);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(count(&drain_out(&op), 0xFA), 1);
        rig.set("resync", json!(false));
        rig.set("reset", json!(true));
        rig.step(&mut op, 0.7, 0.016);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let sent = drain_out(&op);
        assert_eq!(count(&sent, 0xFC), 1);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(count(&drain_out(&op), 0xF8), 0, "no pulses while stopped");
        // Released, it continues from where the clock is (from 0).
        rig.set("reset", json!(false));
        rig.step(&mut op, 0.8, 0.016);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(count(&drain_out(&op), 0xFA), 1);
    }

    #[test]
    fn clock_out_is_silent_offline_and_when_following_midi() {
        let mut rig = Rig::new(&[("send_clock", json!(true)), ("out_device", json!(VIRTUAL))]);
        let mut op = Beat::default();
        rig.cook(&mut op, None, 0.0, 0.0, CookMode::Offline { fps: 60.0 });
        assert!(op.out.is_none(), "sinks do not emit offline");
        rig.step(&mut op, 0.0, 0.0);
        assert!(op.out.is_some());
        rig.set("source", json!("midi"));
        rig.step(&mut op, 0.1, 0.1);
        assert!(op.out.is_none(), "no echo of a followed clock");
    }
}
