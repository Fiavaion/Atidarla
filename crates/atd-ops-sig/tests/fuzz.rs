//! Deterministic fuzzing of SIG cooks: every SIG operator with random
//! parameters and random inputs (control-rate channel sets, audio blocks of
//! odd sizes and rates, empty and missing inputs). Errors are allowed;
//! panics and non-finite outputs are not. Devices point at the built-in
//! virtual/test/null options and loopback, so no hardware is touched.

use std::collections::HashMap;

use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::{ParamKind, Registry};
use atd_gpu::engine::{Channels, Params, SigCtx, SigInputs};
use serde_json::{Value, json};

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
    fn unit(&mut self) -> f64 {
        self.next(1_000_001) as f64 / 1_000_000.0
    }
}

fn random_value(name: &str, kind: &ParamKind, rng: &mut Rng) -> Option<Value> {
    // Keep I/O on safe targets.
    match name {
        "device" | "midi_device" | "out_device" => return Some(json!("virtual")),
        "audio_device" => return Some(json!("test-tone")),
        "host" => return Some(json!("127.0.0.1")),
        "port" => return Some(json!(0)),
        "file" => return None,
        _ => {}
    }
    Some(match kind {
        ParamKind::Float { min, max } => {
            let lo = min.unwrap_or(-1e4);
            let hi = max.unwrap_or(1e4);
            match rng.next(5) {
                0 => json!(lo),
                1 => json!(hi),
                2 => json!(0.0),
                _ => json!(lo + (hi - lo) * rng.unit()),
            }
        }
        ParamKind::Int { min, max } => {
            let lo = min.unwrap_or(-1000);
            let hi = max.unwrap_or(1000);
            json!(lo + rng.next((hi - lo + 1) as u64) as i64)
        }
        ParamKind::Menu { options } => json!(options[rng.next(options.len() as u64) as usize]),
        ParamKind::Toggle | ParamKind::Pulse => json!(rng.next(2) == 1),
        ParamKind::Colour => json!([rng.unit(), rng.unit(), rng.unit(), rng.unit()]),
        ParamKind::Vec { n } => json!((0..*n).map(|_| rng.unit()).collect::<Vec<_>>()),
        ParamKind::String | ParamKind::Path => {
            json!(["", "a", "x y z", "chan1 left", "*", "??"][rng.next(6) as usize])
        }
    })
}

fn random_input(rng: &mut Rng) -> Channels {
    let names = ["a", "b", "left", "right", "x", "level", "chan1"];
    let mut c = Channels::default();
    match rng.next(4) {
        0 => c.set_single(&[], &[]),
        1 => {
            let n = 1 + rng.next(names.len() as u64) as usize;
            let values: Vec<f32> = (0..n).map(|_| (rng.unit() as f32 - 0.5) * 20.0).collect();
            c.set_single(&names[..n], &values);
        }
        _ => {
            let n = 1 + rng.next(2) as usize;
            let samples = [0usize, 1, 7, 735, 4096][rng.next(5) as usize];
            let rate = [0.0f32, 8000.0, 44_100.0, 48_000.0][rng.next(4) as usize];
            c.set_block(&names[2..2 + n], rate, samples);
            for ch in 0..n {
                for v in c.channel_mut(ch) {
                    *v = (rng.unit() as f32 - 0.5) * 2.0;
                }
            }
        }
    }
    c
}

#[test]
fn every_sig_operator_survives_random_inputs() {
    let mut reg = Registry::default();
    atd_ops_sig::register(&mut reg);
    let seed = std::env::var("ATD_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x0DDB_1A5E_5BAD_5EEDu64);
    let mut rng = Rng(seed.max(1));
    let rounds = std::env::var("ATD_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    for def in atd_ops_sig::defs() {
        let schema = (def.schema)();
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, schema.type_id.clone(), "op"),
            },
        )
        .unwrap();
        let srcs: Vec<NodeId> = (0..schema.inputs.len())
            .map(|i| {
                let s = net.alloc_id();
                net.apply(
                    &reg,
                    Edit::AddNode {
                        node: Node::new(s, "sig.constant", format!("s{i}")),
                    },
                )
                .unwrap();
                s
            })
            .collect();
        let mut op = (def.create)();
        let mut out = Channels::default();
        for round in 0..rounds {
            for p in &schema.params {
                if rng.next(2) == 0 {
                    continue;
                }
                if let Some(v) = random_value(&p.name, &p.kind, &mut rng) {
                    let _ = net.apply(
                        &reg,
                        Edit::SetParam {
                            node: id,
                            param: p.name.clone(),
                            value: Some(Param::constant(v)),
                        },
                    );
                }
            }
            let mut sig: HashMap<NodeId, Channels> = HashMap::new();
            let sources: Vec<Option<NodeId>> = srcs
                .iter()
                .map(|s| {
                    if rng.next(5) == 0 {
                        None
                    } else {
                        sig.insert(*s, random_input(&mut rng));
                        Some(*s)
                    }
                })
                .collect();
            let mut ctx = SigCtx {
                inputs: SigInputs::new(&sources, &sig),
                output: &mut out,
                params: Params::new(&net, &reg, &sig, id),
                time: f64::from(round) * 0.02,
                dt: [0.0, 0.001, 0.02, 1.0][rng.next(4) as usize],
                frame: round as u64,
            };
            let _ = op.cook(&mut ctx);
            for c in 0..out.len() {
                assert!(
                    out.channel(c).iter().all(|v| v.is_finite()),
                    "{} round {round}: channel {} is not finite",
                    schema.type_id,
                    out.name(c)
                );
            }
        }
    }
}
