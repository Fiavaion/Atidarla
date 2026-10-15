//! SIG operator definitions (PRD §6, first batch).

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{SigCtx, SigOp};

use crate::Def;
use crate::audio::{AudioFileIn, FFT_SIZES, Spectrum};

pub(crate) fn sig(
    id: &str,
    label: &str,
    summary: &str,
    inputs: &[&str],
    params: Vec<ParamSchema>,
    time_dependent: bool,
) -> OperatorSchema {
    let socket = |name: &str| SocketSchema {
        name: name.into(),
        wire: WireType::Sig,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: format!("sig.{id}"),
        family: OpFamily::Sig,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: inputs.iter().map(|n| socket(n)).collect(),
        outputs: vec![socket("out")],
        capabilities: vec![],
        cook_budget_us: 20,
        time_dependent,
        sink: false,
    }
}

/// Copies input 0 to the output (names and values), or empties the output.
fn pass_through(ctx: &mut SigCtx<'_>) -> bool {
    match ctx.inputs.get(0) {
        Some(input) => {
            ctx.output.copy_from(input);
            true
        }
        None => {
            ctx.output.set_single(&[], &[]);
            false
        }
    }
}

/// Deterministic hash of an integer lattice point to [-1, 1].
fn hash1(i: i64, seed: i64) -> f32 {
    let mut x = (i as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((seed as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// Smooth 1D value noise in [-1, 1].
fn noise1(t: f64, seed: i64) -> f32 {
    let i = t.floor();
    let f = (t - i) as f32;
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash1(i as i64, seed);
    let b = hash1(i as i64 + 1, seed);
    a + (b - a) * u
}

const WAVES: &[&str] = &["sine", "triangle", "square", "ramp"];
const MATH_OPS: &[&str] = &[
    "add", "subtract", "multiply", "divide", "maximum", "minimum",
];

fn wave(kind: &str, x: f64) -> f32 {
    let x = x.rem_euclid(1.0);
    (match kind {
        "triangle" => 1.0 - 4.0 * ((x + 0.25).rem_euclid(1.0) - 0.5).abs(),
        "square" => {
            if x < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        "ramp" => 2.0 * x - 1.0,
        _ => (x * std::f64::consts::TAU).sin(),
    }) as f32
}

struct Constant;
impl SigOp for Constant {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        const NAMES: [&str; 4] = ["name1", "name2", "name3", "name4"];
        const VALUES: [&str; 4] = ["value1", "value2", "value3", "value4"];
        let p = ctx.params;
        let mut w = ctx.output.writer();
        for (n, v) in NAMES.iter().zip(VALUES) {
            let name = p.str(n);
            if !name.is_empty() {
                w.push(name, p.f32(v));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Lfo;
impl SigOp for Lfo {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let x = ctx.time * f64::from(p.f32("frequency")) + f64::from(p.f32("phase"));
        let v = p.f32("offset") + p.f32("amplitude") * wave(p.str("waveform"), x);
        ctx.output.set_single(&["lfo"], &[v]);
        Ok(())
    }
}

struct Noise;
impl SigOp for Noise {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        const NAMES: [&str; 8] = [
            "chan1", "chan2", "chan3", "chan4", "chan5", "chan6", "chan7", "chan8",
        ];
        let p = ctx.params;
        let n = p.i64("channels").clamp(1, 8) as usize;
        let t = ctx.time * f64::from(p.f32("frequency"));
        let seed = p.i64("seed");
        let mut values = [0.0; 8];
        for (c, v) in values.iter_mut().enumerate().take(n) {
            *v = p.f32("offset")
                + p.f32("amplitude") * noise1(t, seed.wrapping_mul(131).wrapping_add(c as i64));
        }
        ctx.output.set_single(&NAMES[..n], &values[..n]);
        Ok(())
    }
}

#[derive(Default)]
struct Timer {
    elapsed: f64,
}
impl SigOp for Timer {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let length = f64::from(p.f32("length")).max(1e-6);
        if p.bool("reset") {
            self.elapsed = 0.0;
        } else if p.bool("play") {
            self.elapsed += ctx.dt;
        }
        let seconds = if p.bool("loop") {
            self.elapsed % length
        } else {
            self.elapsed.min(length)
        };
        let cycles = (self.elapsed / length).floor();
        ctx.output.set_single(
            &["seconds", "fraction", "cycles"],
            &[seconds as f32, (seconds / length) as f32, cycles as f32],
        );
        Ok(())
    }
}

#[derive(Default)]
struct Beat {
    beats: f64,
}
impl SigOp for Beat {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.bool("reset") {
            self.beats = 0.0;
        } else {
            // Integrating keeps the phase continuous when the tempo changes.
            self.beats += ctx.dt * f64::from(p.f32("bpm")) / 60.0;
        }
        let per_bar = p.i64("beats_per_bar").max(1) as f64;
        let beat = self.beats.floor();
        let bar = (self.beats / per_bar).floor();
        ctx.output.set_single(
            &["beat", "beat_phase", "bar", "bar_phase"],
            &[
                beat as f32,
                (self.beats - beat) as f32,
                bar as f32,
                ((self.beats / per_bar) - bar) as f32,
            ],
        );
        Ok(())
    }
}

struct Math;
impl SigOp for Math {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if !pass_through(ctx) {
            return Ok(());
        }
        let b = ctx.inputs.get(1);
        let op = p.str("operation");
        let (fmin, fmax) = (p.f32("from_min"), p.f32("from_max"));
        let (tmin, tmax) = (p.f32("to_min"), p.f32("to_max"));
        let span = fmax - fmin;
        for c in 0..ctx.output.len() {
            let other = b.map(|b| {
                // Channel-wise, or the first channel of B against all of A.
                let i = if c < b.len() { c } else { 0 };
                if b.is_empty() { 0.0 } else { b.last(i) }
            });
            for v in ctx.output.channel_mut(c) {
                if let Some(o) = other {
                    *v = match op {
                        "subtract" => *v - o,
                        "multiply" => *v * o,
                        "divide" => {
                            if o == 0.0 {
                                0.0
                            } else {
                                *v / o
                            }
                        }
                        "maximum" => v.max(o),
                        "minimum" => v.min(o),
                        _ => *v + o,
                    };
                }
                let t = if span == 0.0 { 0.0 } else { (*v - fmin) / span };
                *v = tmin + t * (tmax - tmin);
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Lag {
    state: Vec<f32>,
}
impl SigOp for Lag {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if !pass_through(ctx) {
            return Ok(());
        }
        let (up, down) = (f64::from(p.f32("lag_up")), f64::from(p.f32("lag_down")));
        let fresh = self.state.len() != ctx.output.len();
        self.state.resize(ctx.output.len(), 0.0);
        for (c, s) in self.state.iter_mut().enumerate() {
            let target = ctx.output.last(c);
            if fresh || ctx.dt == 0.0 {
                *s = target;
            } else {
                let tau = if target > *s { up } else { down };
                let k = if tau <= 0.0 {
                    1.0
                } else {
                    1.0 - (-ctx.dt / tau).exp()
                };
                *s += (target - *s) * k as f32;
            }
            ctx.output.channel_mut(c).fill(*s);
        }
        Ok(())
    }
}

#[derive(Default)]
struct Speed {
    acc: Vec<f64>,
}
impl SigOp for Speed {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let reset = ctx.params.bool("reset");
        if !pass_through(ctx) {
            return Ok(());
        }
        self.acc.resize(ctx.output.len(), 0.0);
        for (c, a) in self.acc.iter_mut().enumerate() {
            if reset {
                *a = 0.0;
            } else {
                *a += f64::from(ctx.output.last(c)) * ctx.dt;
            }
            ctx.output.channel_mut(c).fill(*a as f32);
        }
        Ok(())
    }
}

struct Select;
impl SigOp for Select {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let wanted = ctx.params.str("channels");
        let input = ctx.inputs.get(0);
        let mut w = ctx.output.writer();
        if let Some(input) = input {
            for name in wanted.split_whitespace() {
                if let Some(i) = input.index(name) {
                    w.push(name, input.last(i));
                }
            }
        }
        Ok(())
    }
}

struct Merge;
impl SigOp for Merge {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let inputs = ctx.inputs;
        let mut w = ctx.output.writer();
        for i in 0..inputs.len() {
            if let Some(input) = inputs.get(i) {
                for c in 0..input.len() {
                    w.push(input.name(c), input.last(c));
                }
            }
        }
        Ok(())
    }
}

struct Limit;
impl SigOp for Limit {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if !pass_through(ctx) {
            return Ok(());
        }
        let (lo, hi, step) = (p.f32("min"), p.f32("max"), p.f32("step"));
        for c in 0..ctx.output.len() {
            for v in ctx.output.channel_mut(c) {
                if step > 0.0 {
                    *v = (*v / step).round() * step;
                }
                *v = v.clamp(lo.min(hi), hi.max(lo));
            }
        }
        Ok(())
    }
}

pub static DEFS: &[Def] = &[
    Def {
        schema: || {
            let mut params = Vec::new();
            for i in 1..=4 {
                let default_name = if i == 1 { "chan1" } else { "" };
                params.push(
                    ParamSchema::string(&format!("name{i}"), &format!("Name {i}"), default_name)
                        .help("Channel name; leave empty to omit the channel."),
                );
                params.push(
                    ParamSchema::float(
                        &format!("value{i}"),
                        &format!("Value {i}"),
                        0.0,
                        None,
                        None,
                    )
                    .help("Channel value."),
                );
            }
            sig(
                "constant",
                "Constant",
                "Up to four named channels with constant values.",
                &[],
                params,
                false,
            )
        },
        create: || Box::new(Constant),
    },
    Def {
        schema: || {
            sig(
                "lfo",
                "LFO",
                "A low-frequency oscillator: sine, triangle, square or ramp over time. Outputs channel `lfo`.",
                &[],
                vec![
                    ParamSchema::menu("waveform", "Waveform", WAVES, "sine")
                        .help("Shape of one cycle."),
                    ParamSchema::float("frequency", "Frequency", 1.0, None, None)
                        .help("Cycles per second."),
                    ParamSchema::float("amplitude", "Amplitude", 1.0, None, None)
                        .help("Output = offset + amplitude × wave."),
                    ParamSchema::float("offset", "Offset", 0.0, None, None)
                        .help("Added to the scaled wave."),
                    ParamSchema::float("phase", "Phase", 0.0, None, None)
                        .help("Shift in cycles (0–1)."),
                ],
                true,
            )
        },
        create: || Box::new(Lfo),
    },
    Def {
        schema: || {
            sig(
                "noise",
                "Noise",
                "Smooth random channels that wander over time.",
                &[],
                vec![
                    ParamSchema::int("channels", "Channels", 1, Some(1), Some(8))
                        .help("Number of independent channels (chan1…chan8)."),
                    ParamSchema::float("frequency", "Frequency", 1.0, None, None)
                        .help("Changes per second."),
                    ParamSchema::float("amplitude", "Amplitude", 1.0, None, None)
                        .help("Output = offset + amplitude × noise (noise is in −1…1)."),
                    ParamSchema::float("offset", "Offset", 0.0, None, None)
                        .help("Added to the scaled noise."),
                    ParamSchema::int("seed", "Seed", 0, None, None)
                        .help("Selects a different random sequence."),
                ],
                true,
            )
        },
        create: || Box::new(Noise),
    },
    Def {
        schema: || {
            sig(
                "timer",
                "Timer",
                "Counts seconds while playing: `seconds`, `fraction` of the length and completed `cycles`.",
                &[],
                vec![
                    ParamSchema::float("length", "Length", 4.0, Some(0.0), None)
                        .help("Cycle length in seconds."),
                    ParamSchema::toggle("loop", "Loop", true)
                        .help("Wrap around at the end instead of holding."),
                    ParamSchema::toggle("play", "Play", true).help("Advance the timer."),
                    ParamSchema::toggle("reset", "Reset", false)
                        .help("While on, holds the timer at zero."),
                ],
                true,
            )
        },
        create: || Box::<Timer>::default(),
    },
    Def {
        schema: || {
            sig(
                "beat",
                "Beat",
                "A tempo clock: `beat` count, `beat_phase` 0–1 within the beat, `bar` count and `bar_phase`.",
                &[],
                vec![
                    ParamSchema::float("bpm", "BPM", 120.0, Some(0.0), Some(999.0))
                        .help("Tempo in beats per minute; changes keep the phase continuous."),
                    ParamSchema::int("beats_per_bar", "Beats per Bar", 4, Some(1), Some(64))
                        .help("Beats in one bar."),
                    ParamSchema::toggle("reset", "Reset", false)
                        .help("While on, holds the clock at beat zero."),
                ],
                true,
            )
        },
        create: || Box::<Beat>::default(),
    },
    Def {
        schema: || {
            sig(
                "math",
                "Math",
                "Combines A with B channel by channel, then maps the range from→to.",
                &["a", "b"],
                vec![
                    ParamSchema::menu("operation", "Operation", MATH_OPS, "add").help("Applied when B is connected; B's first channel is used against all of A if B has fewer channels."),
                    ParamSchema::float("from_min", "From Min", 0.0, None, None).help("Input value that maps to To Min."),
                    ParamSchema::float("from_max", "From Max", 1.0, None, None).help("Input value that maps to To Max."),
                    ParamSchema::float("to_min", "To Min", 0.0, None, None).help("Output at From Min."),
                    ParamSchema::float("to_max", "To Max", 1.0, None, None).help("Output at From Max."),
                ],
                false,
            )
        },
        create: || Box::new(Math),
    },
    Def {
        schema: || {
            sig(
                "lag",
                "Lag",
                "Smooths changes with separate rise and fall times.",
                &["in"],
                vec![
                    ParamSchema::float("lag_up", "Lag Up", 0.2, Some(0.0), None)
                        .help("Time constant in seconds when rising."),
                    ParamSchema::float("lag_down", "Lag Down", 0.2, Some(0.0), None)
                        .help("Time constant in seconds when falling."),
                ],
                true,
            )
        },
        create: || Box::<Lag>::default(),
    },
    Def {
        schema: || {
            sig(
                "speed",
                "Speed",
                "Integrates each channel over time: the input is a rate per second.",
                &["in"],
                vec![
                    ParamSchema::toggle("reset", "Reset", false)
                        .help("While on, holds the output at zero."),
                ],
                true,
            )
        },
        create: || Box::<Speed>::default(),
    },
    Def {
        schema: || {
            sig(
                "select",
                "Select",
                "Keeps only the named channels.",
                &["in"],
                vec![
                    ParamSchema::string("channels", "Channels", "chan1")
                        .help("Space-separated channel names, in output order."),
                ],
                false,
            )
        },
        create: || Box::new(Select),
    },
    Def {
        schema: || {
            sig(
                "merge",
                "Merge",
                "Combines the channels of up to four inputs, in input order.",
                &["a", "b", "c", "d"],
                vec![],
                false,
            )
        },
        create: || Box::new(Merge),
    },
    Def {
        schema: || {
            sig(
                "limit",
                "Limit",
                "Clamps channels to a range, optionally quantising to a step.",
                &["in"],
                vec![
                    ParamSchema::float("min", "Min", 0.0, None, None).help("Lowest output value."),
                    ParamSchema::float("max", "Max", 1.0, None, None).help("Highest output value."),
                    ParamSchema::float("step", "Step", 0.0, Some(0.0), None)
                        .help("Quantisation step; 0 disables it."),
                ],
                false,
            )
        },
        create: || Box::new(Limit),
    },
    Def {
        schema: || {
            sig(
                "audio_file_in",
                "Audio File In",
                "Plays a WAV file as audio-rate channels (`left`/`right`, or `mono`). Decoding runs off the render thread.",
                &[],
                vec![
                    ParamSchema::string("file", "File", "")
                        .help("Path to a WAV file (PCM or float)."),
                    ParamSchema::toggle("play", "Play", true).help("Advance the playhead."),
                    ParamSchema::toggle("loop", "Loop", true)
                        .help("Restart at the end of the file."),
                    ParamSchema::float("gain", "Gain", 1.0, Some(0.0), None)
                        .help("Scales the samples."),
                ],
                true,
            )
        },
        create: || Box::<AudioFileIn>::default(),
    },
    Def {
        schema: || {
            let mut s = sig(
                "spectrum",
                "Spectrum",
                "Band energies of an audio input (log-spaced `band1`…`bandN`, a full-scale sine reads about 1) plus its RMS `level`.",
                &["in"],
                vec![
                    ParamSchema::menu("fft_size", "FFT Size", FFT_SIZES, "1024").help("Analysis window in samples; larger resolves lower frequencies but reacts more slowly."),
                    ParamSchema::int("bands", "Bands", 8, Some(1), Some(32)).help("Number of log-spaced bands."),
                    ParamSchema::float("min_freq", "Min Frequency", 30.0, Some(1.0), None).help("Lower edge of the first band in Hz."),
                    ParamSchema::float("max_freq", "Max Frequency", 16_000.0, Some(1.0), None).help("Upper edge of the last band in Hz."),
                    ParamSchema::float("smoothing", "Smoothing", 0.5, Some(0.0), Some(0.99)).help("Per-frame smoothing of band values (0 = none)."),
                    ParamSchema::float("gain", "Gain", 1.0, Some(0.0), None).help("Scales the band and level outputs."),
                ],
                true,
            );
            s.cook_budget_us = 200;
            s
        },
        create: || Box::<Spectrum>::default(),
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use atd_core::graph::{Edit, Network, Node, NodeId, Param};
    use atd_core::registry::Registry;
    use atd_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::{Value, json};

    use super::*;

    struct Harness {
        reg: Registry,
        net: Network,
        sig: HashMap<NodeId, Channels>,
    }

    impl Harness {
        fn new() -> Self {
            let mut reg = Registry::default();
            crate::register(&mut reg);
            Self {
                reg,
                net: Network::default(),
                sig: HashMap::new(),
            }
        }

        fn node(&mut self, ty: &str, params: &[(&str, Value)]) -> NodeId {
            let id = self.net.alloc_id();
            let name = self.net.unique_name(ty);
            self.net
                .apply(
                    &self.reg,
                    Edit::AddNode {
                        node: Node::new(id, format!("sig.{ty}"), name),
                    },
                )
                .unwrap();
            for (p, v) in params {
                self.net
                    .apply(
                        &self.reg,
                        Edit::SetParam {
                            node: id,
                            param: (*p).into(),
                            value: Some(Param::constant(v.clone())),
                        },
                    )
                    .unwrap_or_else(|e| panic!("sig.{ty}.{p}: {e}"));
            }
            id
        }

        /// A fake upstream node holding fixed channels.
        fn source(&mut self, chans: &[(&str, f32)]) -> NodeId {
            let id = self.node("constant", &[]);
            let names: Vec<&str> = chans.iter().map(|c| c.0).collect();
            let values: Vec<f32> = chans.iter().map(|c| c.1).collect();
            self.sig.insert(id, Channels::single(&names, &values));
            id
        }

        fn run(
            &self,
            op: &mut dyn SigOp,
            node: NodeId,
            inputs: &[Option<NodeId>],
            time: f64,
            dt: f64,
        ) -> Channels {
            let mut out = Channels::default();
            let mut ctx = SigCtx {
                inputs: SigInputs::new(inputs, &self.sig),
                output: &mut out,
                params: Params::new(&self.net, &self.reg, &self.sig, node),
                time,
                dt,
                frame: 0,
            };
            op.cook(&mut ctx).unwrap();
            out
        }
    }

    fn create(ty: &str) -> Box<dyn SigOp> {
        let def = DEFS
            .iter()
            .find(|d| (d.schema)().type_id == format!("sig.{ty}"))
            .unwrap();
        (def.create)()
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn constant_named_channels() {
        let mut h = Harness::new();
        let n = h.node(
            "constant",
            &[
                ("value1", json!(2.5)),
                ("name2", json!("y")),
                ("value2", json!(-1.0)),
            ],
        );
        let out = h.run(create("constant").as_mut(), n, &[], 0.0, 0.0);
        assert_eq!(out.names().collect::<Vec<_>>(), ["chan1", "y"]);
        assert_eq!(
            (out.value("chan1"), out.value("y")),
            (Some(2.5), Some(-1.0))
        );
    }

    #[test]
    fn lfo_waveforms() {
        let mut h = Harness::new();
        let n = h.node("lfo", &[("frequency", json!(1.0))]);
        let mut op = create("lfo");
        let at = |h: &Harness, op: &mut Box<dyn SigOp>, t: f64| {
            h.run(op.as_mut(), n, &[], t, 0.0).value("lfo").unwrap()
        };
        assert!(
            approx(at(&h, &mut op, 0.25), 1.0),
            "sine peaks at a quarter cycle"
        );
        assert!(approx(at(&h, &mut op, 0.75), -1.0));
        h.net
            .apply(
                &h.reg,
                Edit::SetParam {
                    node: n,
                    param: "waveform".into(),
                    value: Some(Param::constant(json!("square"))),
                },
            )
            .unwrap();
        assert_eq!(at(&h, &mut op, 0.1), 1.0);
        assert_eq!(at(&h, &mut op, 0.6), -1.0);
        h.net
            .apply(
                &h.reg,
                Edit::SetParam {
                    node: n,
                    param: "waveform".into(),
                    value: Some(Param::constant(json!("triangle"))),
                },
            )
            .unwrap();
        assert!(
            approx(at(&h, &mut op, 0.0), 0.0)
                && approx(at(&h, &mut op, 0.25), 1.0)
                && approx(at(&h, &mut op, 0.75), -1.0)
        );
        h.net
            .apply(
                &h.reg,
                Edit::SetParam {
                    node: n,
                    param: "waveform".into(),
                    value: Some(Param::constant(json!("ramp"))),
                },
            )
            .unwrap();
        assert!(approx(at(&h, &mut op, 0.0), -1.0) && approx(at(&h, &mut op, 0.5), 0.0));
    }

    #[test]
    fn noise_is_smooth_bounded_and_seeded() {
        let mut h = Harness::new();
        let n = h.node("noise", &[("channels", json!(2))]);
        let mut op = create("noise");
        let mut prev: Option<f32> = None;
        for i in 0..200 {
            let out = h.run(op.as_mut(), n, &[], i as f64 * 0.01, 0.01);
            assert_eq!(out.len(), 2);
            let v = out.last(0);
            assert!((-1.0..=1.0).contains(&v));
            if let Some(p) = prev {
                assert!((v - p).abs() < 0.1, "smooth: {p} → {v}");
            }
            prev = Some(v);
        }
        let a = h.run(op.as_mut(), n, &[], 3.3, 0.0);
        assert_ne!(a.last(0), a.last(1), "channels are independent");
        h.net
            .apply(
                &h.reg,
                Edit::SetParam {
                    node: n,
                    param: "seed".into(),
                    value: Some(Param::constant(json!(9))),
                },
            )
            .unwrap();
        assert_ne!(
            h.run(op.as_mut(), n, &[], 3.3, 0.0).last(0),
            a.last(0),
            "seed changes the sequence"
        );
    }

    #[test]
    fn timer_loops_and_resets() {
        let mut h = Harness::new();
        let n = h.node("timer", &[("length", json!(1.0))]);
        let mut op = create("timer");
        let mut out = Channels::default();
        for _ in 0..15 {
            out = h.run(op.as_mut(), n, &[], 0.0, 0.1);
        }
        assert!(
            approx(out.value("seconds").unwrap(), 0.5) && approx(out.value("cycles").unwrap(), 1.0)
        );
        h.net
            .apply(
                &h.reg,
                Edit::SetParam {
                    node: n,
                    param: "reset".into(),
                    value: Some(Param::constant(json!(true))),
                },
            )
            .unwrap();
        assert_eq!(
            h.run(op.as_mut(), n, &[], 0.0, 0.1).value("seconds"),
            Some(0.0)
        );
    }

    #[test]
    fn beat_counts_and_keeps_phase_across_tempo_changes() {
        let mut h = Harness::new();
        let n = h.node("beat", &[("bpm", json!(120.0))]);
        let mut op = create("beat");
        let mut out = Channels::default();
        for _ in 0..5 {
            out = h.run(op.as_mut(), n, &[], 0.0, 0.25); // 1.25 s at 2 beats/s
        }
        assert!(
            approx(out.value("beat").unwrap(), 2.0)
                && approx(out.value("beat_phase").unwrap(), 0.5)
        );
        h.net
            .apply(
                &h.reg,
                Edit::SetParam {
                    node: n,
                    param: "bpm".into(),
                    value: Some(Param::constant(json!(60.0))),
                },
            )
            .unwrap();
        out = h.run(op.as_mut(), n, &[], 0.0, 0.25);
        assert!(
            approx(out.value("beat_phase").unwrap(), 0.75),
            "phase continues: {:?}",
            out.value("beat_phase")
        );
        assert!(approx(out.value("bar_phase").unwrap(), 2.75 / 4.0));
    }

    #[test]
    fn math_combines_and_remaps() {
        let mut h = Harness::new();
        let a = h.source(&[("x", 0.25), ("y", 0.5)]);
        let b = h.source(&[("k", 2.0)]);
        let n = h.node("math", &[("operation", json!("multiply"))]);
        let out = h.run(create("math").as_mut(), n, &[Some(a), Some(b)], 0.0, 0.0);
        assert_eq!(
            (out.value("x"), out.value("y")),
            (Some(0.5), Some(1.0)),
            "B's first channel against all of A"
        );
        let n2 = h.node("math", &[("to_min", json!(10.0)), ("to_max", json!(20.0))]);
        let out = h.run(create("math").as_mut(), n2, &[Some(a), None], 0.0, 0.0);
        assert_eq!(out.value("x"), Some(12.5), "range map without B");
        let empty = h.run(create("math").as_mut(), n2, &[None, None], 0.0, 0.0);
        assert!(empty.is_empty());
    }

    #[test]
    fn lag_smooths_towards_input() {
        let mut h = Harness::new();
        let lo = h.source(&[("v", 0.0)]);
        let hi = h.source(&[("v", 1.0)]);
        let n = h.node("lag", &[("lag_up", json!(0.5))]);
        let mut op = create("lag");
        assert_eq!(
            h.run(op.as_mut(), n, &[Some(lo)], 0.0, 0.0).value("v"),
            Some(0.0),
            "starts at the input"
        );
        let v1 = h
            .run(op.as_mut(), n, &[Some(hi)], 0.0, 0.1)
            .value("v")
            .unwrap();
        let v2 = h
            .run(op.as_mut(), n, &[Some(hi)], 0.0, 0.1)
            .value("v")
            .unwrap();
        assert!(v1 > 0.0 && v1 < v2 && v2 < 1.0, "{v1} {v2}");
        assert!(approx(v1, 1.0 - (-0.2f32).exp()));
    }

    #[test]
    fn speed_integrates() {
        let mut h = Harness::new();
        let rate = h.source(&[("r", 2.0)]);
        let n = h.node("speed", &[]);
        let mut op = create("speed");
        let mut out = Channels::default();
        for _ in 0..10 {
            out = h.run(op.as_mut(), n, &[Some(rate)], 0.0, 0.1);
        }
        assert!(approx(out.value("r").unwrap(), 2.0));
    }

    #[test]
    fn select_merge_limit() {
        let mut h = Harness::new();
        let a = h.source(&[("x", 1.0), ("y", 2.0), ("z", 3.0)]);
        let b = h.source(&[("w", 7.4)]);
        let s = h.node("select", &[("channels", json!("z x missing"))]);
        let out = h.run(create("select").as_mut(), s, &[Some(a)], 0.0, 0.0);
        assert_eq!(out.names().collect::<Vec<_>>(), ["z", "x"]);
        let m = h.node("merge", &[]);
        let out = h.run(
            create("merge").as_mut(),
            m,
            &[Some(a), None, Some(b), None],
            0.0,
            0.0,
        );
        assert_eq!(out.names().collect::<Vec<_>>(), ["x", "y", "z", "w"]);
        let l = h.node("limit", &[("max", json!(10.0)), ("step", json!(0.5))]);
        let out = h.run(create("limit").as_mut(), l, &[Some(b)], 0.0, 0.0);
        assert_eq!(out.value("w"), Some(7.5));
    }

    #[test]
    fn steady_state_cooks_reuse_the_output_buffer() {
        let mut h = Harness::new();
        let n = h.node("lfo", &[]);
        let mut op = create("lfo");
        let mut out = Channels::default();
        let mut cook = |out: &mut Channels, t| {
            let mut ctx = SigCtx {
                inputs: SigInputs::new(&[], &h.sig),
                output: out,
                params: Params::new(&h.net, &h.reg, &h.sig, n),
                time: t,
                dt: 0.016,
                frame: 0,
            };
            op.cook(&mut ctx).unwrap();
        };
        cook(&mut out, 0.0);
        let before = format!("{:p}", out.channel(0));
        cook(&mut out, 0.1);
        assert_eq!(
            format!("{:p}", out.channel(0)),
            before,
            "no reallocation between cooks"
        );
    }
}
