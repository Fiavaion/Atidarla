//! Pattern (PRD §6, SIG generators): one channel holding an array of
//! samples shaped by a waveform, for lookups, bar graphs and per-instance
//! values (Channel Image turns it into a texture).

use lsr_core::registry::ParamSchema;
use lsr_gpu::engine::{SigCtx, SigOp};

use crate::Def;
use crate::ops::{noise1, sig, wave};

const SHAPES: &[&str] = &["sine", "triangle", "square", "ramp", "noise", "random"];

#[derive(Default)]
struct Pattern;

/// Integer hash to [0, 1) (SplitMix64 finaliser).
fn hash01(i: i64, seed: i64) -> f32 {
    let mut z = (i as u64)
        .wrapping_add((seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

impl SigOp for Pattern {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let n = p.i64("length").clamp(1, 4096) as usize;
        let cycles = f64::from(p.f32("cycles"));
        let phase = f64::from(p.f32("phase")) + ctx.time * f64::from(p.f32("speed"));
        let (amp, offset) = (p.f32("amplitude"), p.f32("offset"));
        let seed = p.i64("seed");
        let shape = p.str("shape");
        ctx.output.set_block(&["pattern"], 0.0, n);
        for (i, out) in ctx.output.channel_mut(0).iter_mut().enumerate() {
            // Position along the pattern: 0 at the first sample, `cycles` at
            // the end (exclusive), so whole cycles tile seamlessly.
            let x = i as f64 / n as f64 * cycles + phase;
            let v = match shape {
                "noise" => noise1(x, seed),
                "random" => hash01(i as i64, seed) * 2.0 - 1.0,
                other => wave(other, x),
            };
            *out = offset + amp * v;
        }
        Ok(())
    }
}

pub static DEFS5: &[Def] = &[Def {
    schema: || {
        sig(
            "pattern",
            "Pattern",
            "One channel of Length samples shaped by a waveform over Cycles; Speed scrolls it over time. Feed Channel Image to turn it into a texture, or index it from other operators.",
            &[],
            vec![
                ParamSchema::menu("shape", "Shape", SHAPES, "sine")
                    .help("Waveform; `noise` is smooth value noise, `random` an independent value per sample."),
                ParamSchema::int("length", "Length", 64, Some(1), Some(4096))
                    .help("Number of samples."),
                ParamSchema::float("cycles", "Cycles", 1.0, None, None)
                    .help("Waveform cycles across the whole pattern."),
                ParamSchema::float("phase", "Phase", 0.0, None, None)
                    .help("Shift in cycles."),
                ParamSchema::float("speed", "Speed", 0.0, None, None)
                    .help("Phase change per second (cycles); 0 holds the pattern still."),
                ParamSchema::float("amplitude", "Amplitude", 1.0, None, None)
                    .help("Scale of the waveform (it spans −1…1 before scaling)."),
                ParamSchema::float("offset", "Offset", 0.0, None, None)
                    .help("Added after scaling."),
                ParamSchema::int("seed", "Seed", 0, None, None)
                    .help("Seed for `noise` and `random`."),
            ],
            true,
        )
    },
    create: || Box::<Pattern>::default(),
}];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lsr_core::graph::{Edit, Network, Node, Param};
    use lsr_core::registry::Registry;
    use lsr_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::{Value, json};

    use super::*;

    fn run(params: &[(&str, Value)], time: f64) -> Vec<f32> {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.pattern", "p1"),
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
        let sig = HashMap::new();
        let mut out = Channels::default();
        let mut ctx = SigCtx {
            dat: lsr_gpu::engine::DatIo::none(),
            inputs: SigInputs::new(&[], &sig),
            output: &mut out,
            params: Params::new(&net, &reg, &sig, id),
            time,
            dt: 0.016,
            frame: 0,
        };
        Pattern.cook(&mut ctx).unwrap();
        assert_eq!(out.names().collect::<Vec<_>>(), ["pattern"]);
        out.channel(0).to_vec()
    }

    #[test]
    fn ramp_spans_one_cycle() {
        let v = run(&[("shape", json!("ramp")), ("length", json!(4))], 0.0);
        assert_eq!(v, [-1.0, -0.5, 0.0, 0.5]);
    }

    #[test]
    fn cycles_amplitude_offset_and_speed() {
        let v = run(
            &[
                ("shape", json!("square")),
                ("length", json!(8)),
                ("cycles", json!(2.0)),
                ("amplitude", json!(0.5)),
                ("offset", json!(0.5)),
            ],
            0.0,
        );
        assert_eq!(v, [1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
        // Half a cycle per second for 1 s inverts a square wave.
        let moved = run(
            &[
                ("shape", json!("square")),
                ("length", json!(8)),
                ("cycles", json!(2.0)),
                ("speed", json!(0.5)),
            ],
            1.0,
        );
        assert_eq!(moved[0], -1.0);
    }

    #[test]
    fn random_is_seeded_and_bounded() {
        let a = run(&[("shape", json!("random")), ("length", json!(256))], 0.0);
        let b = run(&[("shape", json!("random")), ("length", json!(256))], 5.0);
        let c = run(
            &[
                ("shape", json!("random")),
                ("length", json!(256)),
                ("seed", json!(7)),
            ],
            0.0,
        );
        assert_eq!(a, b, "independent of time");
        assert_ne!(a, c, "seed changes it");
        assert!(a.iter().all(|v| (-1.0..1.0).contains(v)));
        let mean = a.iter().sum::<f32>() / a.len() as f32;
        assert!(mean.abs() < 0.15, "roughly centred: {mean}");
    }
}
