//! SIG operators, batch 2: triggers, filters, history, onset detection and
//! OSC (PRD §6). Network I/O runs on `lsr-io` threads, never on the cook.

use lsr_core::registry::ParamSchema;
use lsr_gpu::engine::{SigCtx, SigOp};
use lsr_io::osc::{OscReceiver, OscSender};

use crate::Def;
use crate::audio::{FFT_SIZES, FftWindow};
use crate::ops::sig;

const FILTERS: &[&str] = &["lowpass", "highpass"];
const ENVELOPES: &[&str] = &["rms", "peak"];

/// First channel's latest value of input 0.
fn first(ctx: &SigCtx<'_>) -> Option<f32> {
    ctx.inputs
        .get(0)
        .filter(|c| !c.is_empty())
        .map(|c| c.last(0))
}

/// Attack/decay envelope fired by a rising edge.
#[derive(Default)]
struct Trigger {
    was_high: bool,
    was_fired: bool,
    since: Option<f64>,
}

impl SigOp for Trigger {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let high = first(ctx).is_some_and(|v| v > p.f32("threshold"));
        let fire = p.bool("fire");
        if (high && !self.was_high) || (fire && !self.was_fired) {
            self.since = Some(0.0);
        } else if let Some(t) = &mut self.since {
            *t += ctx.dt;
        }
        self.was_high = high;
        self.was_fired = fire;
        let (attack, decay) = (
            f64::from(p.f32("attack")).max(0.0),
            f64::from(p.f32("decay")).max(0.0),
        );
        let v = match self.since {
            Some(t) if t < attack => t / attack,
            Some(t) if t < attack + decay => 1.0 - (t - attack) / decay,
            _ => 0.0,
        };
        ctx.output.set_single(&["trigger"], &[v as f32]);
        Ok(())
    }
}

/// Counts rising edges.
#[derive(Default)]
struct Count {
    count: f64,
    was_high: bool,
}

impl SigOp for Count {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let high = first(ctx).is_some_and(|v| v > p.f32("threshold"));
        if p.bool("reset") {
            self.count = 0.0;
        } else if high && !self.was_high {
            self.count += 1.0;
        }
        self.was_high = high;
        ctx.output.set_single(&["count"], &[self.count as f32]);
        Ok(())
    }
}

/// One-pole low/high-pass per channel.
#[derive(Default)]
struct Filter {
    state: Vec<f32>,
}

impl SigOp for Filter {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        ctx.output.copy_from(input);
        let fresh = self.state.len() != input.len();
        self.state.resize(input.len(), 0.0);
        let k = 1.0 - (-std::f64::consts::TAU * f64::from(p.f32("cutoff").max(0.0)) * ctx.dt).exp();
        let high = p.str("type") == "highpass";
        for (c, s) in self.state.iter_mut().enumerate() {
            for v in ctx.output.channel_mut(c) {
                if fresh {
                    *s = *v;
                }
                *s += (*v - *s) * k as f32;
                *v = if high { *v - *s } else { *s };
            }
        }
        Ok(())
    }
}

/// Follows the level of an audio-rate input with attack and release times.
#[derive(Default)]
struct Envelope {
    state: Vec<f32>,
}

impl SigOp for Envelope {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        let peak_mode = p.str("method") == "peak";
        let (attack, release) = (f64::from(p.f32("attack")), f64::from(p.f32("release")));
        self.state.resize(input.len(), 0.0);
        let mut w = ctx.output.writer();
        for (c, s) in self.state.iter_mut().enumerate() {
            let samples = input.channel(c);
            if samples.is_empty() {
                w.push(input.name(c), *s);
                continue;
            }
            let level = if peak_mode {
                samples.iter().fold(0.0f32, |m, v| m.max(v.abs()))
            } else {
                (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt()
            };
            let tau = if level > *s { attack } else { release };
            let k = if tau <= 0.0 || ctx.dt == 0.0 {
                1.0
            } else {
                1.0 - (-ctx.dt / tau).exp()
            };
            *s += (level - *s) * k as f32;
            w.push(input.name(c), *s);
        }
        Ok(())
    }
}

/// Keeps the recent history of each channel as a multi-sample channel.
#[derive(Default)]
struct Trail {
    history: Vec<f32>,
    channels: usize,
    filled: usize,
    head: usize,
    length: usize,
}

impl SigOp for Trail {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let length = ctx.params.i64("length").clamp(2, 4096) as usize;
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        if input.len() != self.channels || length != self.length {
            self.channels = input.len();
            self.length = length;
            self.history.clear();
            self.history.resize(self.channels * length, 0.0);
            self.filled = 0;
            self.head = 0;
            // The output grows to `length` samples as history fills.
            ctx.output.reserve(self.channels * length);
        }
        for c in 0..self.channels {
            self.history[c * length + self.head] = input.last(c);
        }
        self.head = (self.head + 1) % length;
        self.filled = (self.filled + 1).min(length);
        let n = self.filled;
        ctx.output.set_block_like(input, 0.0, n);
        for c in 0..self.channels {
            let ring = &self.history[c * length..(c + 1) * length];
            for (i, out) in ctx.output.channel_mut(c).iter_mut().enumerate() {
                // Oldest first, newest last.
                *out = ring[(self.head + length - n + i) % length];
            }
        }
        Ok(())
    }
}

/// Onset (beat) detection by spectral flux with an adaptive threshold.
#[derive(Default)]
struct Onset {
    win: FftWindow,
    prev: Vec<f32>,
    flux_history: Vec<f32>,
    pos: usize,
    since_onset: f64,
}

impl SigOp for Onset {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        const HISTORY: usize = 43; // ~0.7 s at 60 fps
        let p = ctx.params;
        let size: usize = p.str("fft_size").parse().unwrap_or(1024);
        self.win.prepare(size);
        let got_audio = ctx.inputs.get(0).is_some_and(|c| c.samples() > 0);
        if let Some(input) = ctx.inputs.get(0) {
            self.win.push(input);
        }
        let half = self.win.size() / 2;
        self.prev.resize(half, 0.0);
        self.flux_history.resize(HISTORY, 0.0);
        let norm = 4.0 / size as f32;
        let mut flux = 0.0;
        let spectrum = self.win.analyse();
        for (prev, bin) in self.prev.iter_mut().zip(&spectrum[..half]) {
            let m = bin.norm() * norm;
            flux += (m - *prev).max(0.0);
            *prev = m;
        }
        let mean = self.flux_history.iter().sum::<f32>() / HISTORY as f32;
        self.since_onset += ctx.dt;
        let onset = got_audio
            && flux > mean * p.f32("sensitivity").max(1.0) + p.f32("floor")
            && self.since_onset >= f64::from(p.f32("min_interval"));
        if onset {
            self.since_onset = 0.0;
        }
        if got_audio {
            self.flux_history[self.pos] = flux;
            self.pos = (self.pos + 1) % HISTORY;
        }
        ctx.output
            .set_single(&["onset", "flux"], &[if onset { 1.0 } else { 0.0 }, flux]);
        Ok(())
    }
}

/// Latest numeric value of every OSC address received on a port.
#[derive(Default)]
struct OscIn {
    rx: Option<OscReceiver>,
    port: i64,
    local: bool,
}

impl SigOp for OscIn {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A live input is idle in an offline render (FR-H1): no port is
            // opened and nothing has arrived.
            ctx.output.set_single(&[], &[]);
            return Ok(());
        }
        let port = p.i64("port").clamp(0, 65_535);
        let local = p.str("listen") == "local";
        if self.rx.is_none() || port != self.port || local != self.local {
            self.rx = None;
            self.port = port;
            self.local = local;
            let bound = if local {
                OscReceiver::bind_loopback(port as u16)
            } else {
                OscReceiver::bind(port as u16)
            };
            self.rx = Some(bound.map_err(|e| format!("cannot listen on UDP port {port}: {e}"))?);
        }
        let prefix = p.str("filter");
        let rx = self.rx.as_ref().expect("bound above");
        rx.with_values(|values| {
            let mut w = ctx.output.writer();
            for (name, v) in values {
                if name.starts_with(prefix) {
                    w.push(name, *v);
                }
            }
        });
        match rx.ignored() {
            0 => Ok(()),
            n => Err(format!(
                "more than {} OSC addresses received; {n} messages to new addresses ignored",
                lsr_io::osc::MAX_ADDRESSES
            )),
        }
    }
}

/// Sends input channels as OSC messages; passes the input through.
#[derive(Default)]
struct OscOut {
    tx: Option<OscSender>,
    key: (String, i64, String),
}

impl SigOp for OscOut {
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
        let (host, port, prefix) = (
            p.str("host"),
            p.i64("port").clamp(1, 65_535),
            p.str("prefix"),
        );
        if self.tx.is_none() || self.key.0 != host || self.key.1 != port || self.key.2 != prefix {
            self.tx = None;
            self.key = (host.to_owned(), port, prefix.to_owned());
            self.tx = Some(
                OscSender::new(host, port as u16, prefix)
                    .map_err(|e| format!("cannot send to {host}:{port}: {e}"))?,
            );
        }
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        let tx = self.tx.as_ref().expect("created above");
        tx.publish((0..input.len()).map(|c| (input.name(c), input.last(c))));
        ctx.output.copy_from(input);
        Ok(())
    }
}

pub static DEFS2: &[Def] = &[
    Def {
        schema: || {
            sig(
                "trigger",
                "Trigger",
                "An attack/decay envelope fired when the input rises above a threshold or Fire is switched on. Outputs `trigger`.",
                &["in"],
                vec![
                    ParamSchema::float("threshold", "Threshold", 0.5, None, None)
                        .help("Input value that counts as high."),
                    ParamSchema::float("attack", "Attack", 0.01, Some(0.0), None)
                        .help("Seconds to rise to 1."),
                    ParamSchema::float("decay", "Decay", 0.5, Some(0.0), None)
                        .help("Seconds to fall back to 0."),
                    ParamSchema::toggle("fire", "Fire", false)
                        .help("Turning this on fires the trigger."),
                ],
                true,
            )
        },
        create: || Box::<Trigger>::default(),
    },
    Def {
        schema: || {
            sig(
                "count",
                "Count",
                "Counts how many times the input rises above a threshold. Outputs `count`.",
                &["in"],
                vec![
                    ParamSchema::float("threshold", "Threshold", 0.5, None, None)
                        .help("Input value that counts as high."),
                    ParamSchema::toggle("reset", "Reset", false)
                        .help("While on, holds the count at zero."),
                ],
                false,
            )
        },
        create: || Box::<Count>::default(),
    },
    Def {
        schema: || {
            sig(
                "filter",
                "Filter",
                "One-pole low-pass (smooth) or high-pass (changes only) filter per channel.",
                &["in"],
                vec![
                    ParamSchema::menu("type", "Type", FILTERS, "lowpass")
                        .help("Low-pass keeps slow movement; high-pass keeps fast changes."),
                    ParamSchema::float("cutoff", "Cutoff", 2.0, Some(0.0), None)
                        .help("Cut-off frequency in Hz."),
                ],
                true,
            )
        },
        create: || Box::<Filter>::default(),
    },
    Def {
        schema: || {
            sig(
                "envelope",
                "Envelope",
                "Follows the loudness of an audio input per channel (RMS or peak) with attack and release.",
                &["in"],
                vec![
                    ParamSchema::menu("method", "Method", ENVELOPES, "rms")
                        .help("RMS follows perceived loudness; peak reacts to transients."),
                    ParamSchema::float("attack", "Attack", 0.01, Some(0.0), None)
                        .help("Seconds to follow a rise."),
                    ParamSchema::float("release", "Release", 0.3, Some(0.0), None)
                        .help("Seconds to follow a fall."),
                ],
                true,
            )
        },
        create: || Box::<Envelope>::default(),
    },
    Def {
        schema: || {
            sig(
                "trail",
                "Trail",
                "Keeps the recent history of each channel: each output channel holds the last N frames, oldest first.",
                &["in"],
                vec![
                    ParamSchema::int("length", "Length", 120, Some(2), Some(4096))
                        .help("Frames of history."),
                ],
                true,
            )
        },
        create: || Box::<Trail>::default(),
    },
    Def {
        schema: || {
            let mut s = sig(
                "onset",
                "Onset",
                "Detects onsets (beats, hits) in an audio input by spectral flux. Outputs `onset` (1 on detection frames) and `flux`.",
                &["in"],
                vec![
                    ParamSchema::menu("fft_size", "FFT Size", FFT_SIZES, "1024").help("Analysis window in samples."),
                    ParamSchema::float("sensitivity", "Sensitivity", 1.5, Some(1.0), None).help("How far above the recent average the flux must rise (lower = more onsets)."),
                    ParamSchema::float("floor", "Floor", 0.01, Some(0.0), None).help("Minimum flux to count, ignoring near-silence."),
                    ParamSchema::float("min_interval", "Min Interval", 0.1, Some(0.0), None).help("Seconds between onsets."),
                ],
                true,
            );
            s.cook_budget_us = 200;
            s
        },
        create: || Box::<Onset>::default(),
    },
    Def {
        schema: || {
            sig(
                "osc_in",
                "OSC In",
                "Receives OSC on a UDP port; each numeric address becomes a channel (`/a/b` → `a_b`) holding its latest value.",
                &[],
                vec![
                    ParamSchema::int("port", "Port", 9000, Some(0), Some(65_535))
                        .help("UDP port to listen on."),
                    ParamSchema::menu("listen", "Listen", &["all", "local"], "all")
                        .help("`all`: every network interface, so other computers can send; `local`: this computer only, which nothing on the network can reach and Windows Firewall does not ask about."),
                    ParamSchema::string("filter", "Filter", "")
                        .help("Only channels starting with this text."),
                ],
                true,
            )
        },
        create: || Box::<OscIn>::default(),
    },
    Def {
        schema: || {
            let mut s = sig(
                "osc_out",
                "OSC Out",
                "Sends each input channel as an OSC float message `<prefix>/<channel>` whenever the input changes, and passes it through.",
                &["in"],
                vec![
                    ParamSchema::string("host", "Host", "127.0.0.1")
                        .help("Destination host name or IP address."),
                    ParamSchema::int("port", "Port", 9001, Some(1), Some(65_535))
                        .help("Destination UDP port."),
                    ParamSchema::string("prefix", "Prefix", "/lsr")
                        .help("Address prefix for every message."),
                ],
                false,
            );
            s.sink = true;
            s
        },
        create: || Box::<OscOut>::default(),
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::UdpSocket;
    use std::time::{Duration, Instant};

    use lsr_core::graph::{Edit, Network, Node, NodeId, Param};
    use lsr_core::registry::Registry;
    use lsr_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::{Value, json};

    use super::*;

    struct H {
        reg: Registry,
        net: Network,
        sig: HashMap<NodeId, Channels>,
        src: NodeId,
    }

    impl H {
        fn new() -> Self {
            let mut reg = Registry::default();
            crate::register(&mut reg);
            let mut net = Network::default();
            let src = net.alloc_id();
            net.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(src, "sig.constant", "src"),
                },
            )
            .unwrap();
            Self {
                reg,
                net,
                sig: HashMap::new(),
                src,
            }
        }

        fn node(&mut self, ty: &str, params: &[(&str, Value)]) -> NodeId {
            let id = self.net.alloc_id();
            let name = self.net.unique_name("n");
            self.net
                .apply(
                    &self.reg,
                    Edit::AddNode {
                        node: Node::new(id, format!("sig.{ty}"), name),
                    },
                )
                .unwrap();
            for (p, v) in params {
                self.set(id, p, v.clone());
            }
            id
        }

        fn set(&mut self, id: NodeId, p: &str, v: Value) {
            self.net
                .apply(
                    &self.reg,
                    Edit::SetParam {
                        node: id,
                        param: p.into(),
                        value: Some(Param::constant(v)),
                    },
                )
                .unwrap();
        }

        fn input(&mut self, c: Channels) {
            self.sig.insert(self.src, c);
        }

        fn cook(
            &self,
            op: &mut dyn SigOp,
            id: NodeId,
            out: &mut Channels,
            dt: f64,
        ) -> Result<(), String> {
            let sources = [Some(self.src)];
            let mut ctx = SigCtx {
                dat: lsr_gpu::engine::DatIo::none(),
                inputs: SigInputs::new(&sources, &self.sig),
                output: out,
                params: Params::new(&self.net, &self.reg, &self.sig, id),
                time: 0.0,
                dt,
                frame: 0,
            };
            op.cook(&mut ctx)
        }
    }

    fn op(ty: &str) -> Box<dyn SigOp> {
        let def = DEFS2
            .iter()
            .find(|d| (d.schema)().type_id == format!("sig.{ty}"))
            .unwrap();
        (def.create)()
    }

    #[test]
    fn trigger_fires_on_rising_edges() {
        let mut h = H::new();
        let id = h.node("trigger", &[("attack", json!(0.1)), ("decay", json!(0.2))]);
        let mut t = op("trigger");
        let mut out = Channels::default();
        h.input(Channels::single(&["x"], &[0.0]));
        h.cook(t.as_mut(), id, &mut out, 0.05).unwrap();
        assert_eq!(out.value("trigger"), Some(0.0));
        h.input(Channels::single(&["x"], &[1.0]));
        h.cook(t.as_mut(), id, &mut out, 0.05).unwrap();
        assert_eq!(out.value("trigger"), Some(0.0), "fires at t = 0");
        h.cook(t.as_mut(), id, &mut out, 0.05).unwrap();
        assert!(
            (out.last(0) - 0.5).abs() < 1e-4,
            "half-way through the attack"
        );
        for _ in 0..3 {
            h.cook(t.as_mut(), id, &mut out, 0.05).unwrap();
        }
        assert!(
            (out.last(0) - 0.5).abs() < 1e-4,
            "half-way through the decay: {}",
            out.last(0)
        );
        for _ in 0..4 {
            h.cook(t.as_mut(), id, &mut out, 0.05).unwrap();
        }
        assert_eq!(out.last(0), 0.0, "held high does not refire");
    }

    #[test]
    fn count_counts_rising_edges() {
        let mut h = H::new();
        let id = h.node("count", &[]);
        let mut c = op("count");
        let mut out = Channels::default();
        for v in [0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 1.0] {
            h.input(Channels::single(&["x"], &[v]));
            h.cook(c.as_mut(), id, &mut out, 0.01).unwrap();
        }
        assert_eq!(out.value("count"), Some(3.0));
        h.set(id, "reset", json!(true));
        h.cook(c.as_mut(), id, &mut out, 0.01).unwrap();
        assert_eq!(out.value("count"), Some(0.0));
    }

    #[test]
    fn filter_low_and_high_pass() {
        let mut h = H::new();
        let id = h.node("filter", &[("cutoff", json!(1.0))]);
        let mut f = op("filter");
        let mut out = Channels::default();
        h.input(Channels::single(&["x"], &[0.0]));
        h.cook(f.as_mut(), id, &mut out, 0.01).unwrap();
        h.input(Channels::single(&["x"], &[1.0]));
        h.cook(f.as_mut(), id, &mut out, 0.01).unwrap();
        let low = out.last(0);
        assert!(low > 0.0 && low < 0.1, "low-pass moves slowly: {low}");
        h.set(id, "type", json!("highpass"));
        h.cook(f.as_mut(), id, &mut out, 0.01).unwrap();
        assert!(
            out.last(0) > 0.8,
            "high-pass keeps the step: {}",
            out.last(0)
        );
    }

    #[test]
    fn envelope_follows_audio_level() {
        let mut h = H::new();
        let id = h.node("envelope", &[("attack", json!(0.0))]);
        let mut e = op("envelope");
        let mut out = Channels::default();
        let mut block = Channels::default();
        block.set_block(&["mono"], 48_000.0, 480);
        for (i, s) in block.channel_mut(0).iter_mut().enumerate() {
            *s = if i % 2 == 0 { 0.5 } else { -0.5 };
        }
        h.input(block.clone());
        h.cook(e.as_mut(), id, &mut out, 0.01).unwrap();
        assert!(
            (out.value("mono").unwrap() - 0.5).abs() < 1e-4,
            "RMS of ±0.5"
        );
        h.set(id, "method", json!("peak"));
        h.cook(e.as_mut(), id, &mut out, 0.01).unwrap();
        assert!((out.last(0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn trail_keeps_history_oldest_first() {
        let mut h = H::new();
        let id = h.node("trail", &[("length", json!(3))]);
        let mut t = op("trail");
        let mut out = Channels::default();
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.input(Channels::single(&["x"], &[v]));
            h.cook(t.as_mut(), id, &mut out, 0.01).unwrap();
        }
        assert_eq!(out.channel(0), [2.0, 3.0, 4.0]);
        assert_eq!(out.name(0), "x");
    }

    #[test]
    fn onset_detects_a_hit_after_silence() {
        let mut h = H::new();
        let id = h.node("onset", &[("min_interval", json!(0.0))]);
        let mut o = op("onset");
        let mut out = Channels::default();
        let mut block = Channels::default();
        block.set_block(&["mono"], 48_000.0, 800);
        h.input(block.clone());
        for _ in 0..30 {
            h.cook(o.as_mut(), id, &mut out, 1.0 / 60.0).unwrap();
            assert_eq!(out.value("onset"), Some(0.0), "silence has no onsets");
        }
        for (i, s) in block.channel_mut(0).iter_mut().enumerate() {
            *s = (i as f32 * 0.3).sin() * 0.8;
        }
        h.input(block);
        h.cook(o.as_mut(), id, &mut out, 1.0 / 60.0).unwrap();
        assert_eq!(
            out.value("onset"),
            Some(1.0),
            "a sudden tone is an onset (flux {:?})",
            out.value("flux")
        );
    }

    #[test]
    fn osc_in_and_out_over_loopback() {
        let mut h = H::new();
        // OSC In on a free port.
        let port = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let rx_id = h.node(
            "osc_in",
            &[("port", json!(port)), ("listen", json!("local"))],
        );
        let mut rx = op("osc_in");
        let mut rx_out = Channels::default();
        h.cook(rx.as_mut(), rx_id, &mut rx_out, 0.01).unwrap();

        // OSC Out sends the input channels there.
        let tx_id = h.node(
            "osc_out",
            &[("port", json!(port)), ("prefix", json!("/test"))],
        );
        let mut tx = op("osc_out");
        let mut tx_out = Channels::default();
        h.input(Channels::single(&["level", "beat"], &[0.5, 2.0]));
        h.cook(tx.as_mut(), tx_id, &mut tx_out, 0.01).unwrap();
        assert_eq!(tx_out.value("level"), Some(0.5), "passes the input through");

        let deadline = Instant::now() + Duration::from_secs(5);
        while rx_out.len() < 2 {
            assert!(Instant::now() < deadline, "OSC arrived");
            std::thread::sleep(Duration::from_millis(5));
            h.cook(rx.as_mut(), rx_id, &mut rx_out, 0.01).unwrap();
        }
        assert_eq!(
            (rx_out.value("test_level"), rx_out.value("test_beat")),
            (Some(0.5), Some(2.0))
        );

        h.set(rx_id, "filter", json!("test_b"));
        h.cook(rx.as_mut(), rx_id, &mut rx_out, 0.01).unwrap();
        assert_eq!(rx_out.names().collect::<Vec<_>>(), ["test_beat"]);
    }
}
