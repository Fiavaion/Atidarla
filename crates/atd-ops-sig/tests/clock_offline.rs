//! FR-H1 for the beat clock: in an offline render (`CookMode::Offline`)
//! frame `i` is a function of the project and `i` alone. The internal clock,
//! its tap/nudge/resync/reset, a synced LFO and a synced Timer give the same
//! channels, bit for bit, on two runs; a MIDI source and MIDI clock out are
//! idle (no device is touched).

use std::collections::HashMap;

use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Channels, CookMode, Params, SigCtx, SigInputs, SigOp};
use serde_json::{Value, json};

const FPS: f64 = 60.0;
const FRAMES: usize = 1200;

struct Chain {
    reg: Registry,
    net: Network,
    clock: NodeId,
    lfo: NodeId,
    timer: NodeId,
}

fn create(type_id: &str) -> Box<dyn SigOp> {
    let def = atd_ops_sig::defs()
        .find(|d| (d.schema)().type_id == type_id)
        .expect("operator exists");
    (def.create)()
}

impl Chain {
    fn new(clock_params: &[(&str, Value)]) -> Self {
        let mut reg = Registry::default();
        atd_ops_sig::register(&mut reg);
        let mut net = Network::default();
        let mut add = |ty: &str, name: &str, params: &[(&str, Value)]| {
            let id = net.alloc_id();
            net.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(id, ty, name),
                },
            )
            .unwrap();
            for (k, v) in params {
                net.apply(
                    &reg,
                    Edit::SetParam {
                        node: id,
                        param: (*k).into(),
                        value: Some(Param::constant(v.clone())),
                    },
                )
                .unwrap_or_else(|e| panic!("{ty}.{k}: {e}"));
            }
            id
        };
        let clock = add("sig.beat", "clock", clock_params);
        let lfo = add(
            "sig.lfo",
            "lfo",
            &[
                ("sync", json!("beats")),
                ("frequency", json!(0.25)),
                ("waveform", json!("triangle")),
            ],
        );
        let timer = add(
            "sig.timer",
            "timer",
            &[("sync", json!("beats")), ("length", json!(3.0))],
        );
        Self {
            reg,
            net,
            clock,
            lfo,
            timer,
        }
    }

    fn set(&mut self, name: &str, value: Value) {
        self.net
            .apply(
                &self.reg,
                Edit::SetParam {
                    node: self.clock,
                    param: name.into(),
                    value: Some(Param::constant(value)),
                },
            )
            .unwrap();
    }

    /// The frames' bit patterns: clock, LFO and timer channels. A scripted
    /// player taps, nudges, resyncs and resets at fixed frames.
    fn run(&mut self) -> Vec<u32> {
        let mut ops: Vec<(NodeId, Box<dyn SigOp>)> = vec![
            (self.clock, create("sig.beat")),
            (self.lfo, create("sig.lfo")),
            (self.timer, create("sig.timer")),
        ];
        let mut sig: HashMap<NodeId, Channels> = HashMap::new();
        let mut bits = Vec::new();
        for frame in 0..FRAMES {
            match frame {
                100 | 104 | 108 | 112 => self.set("tap", json!(true)),
                101 | 105 | 109 | 113 => self.set("tap", json!(false)),
                300 => self.set("nudge", json!(1.0)),
                360 => self.set("nudge", json!(0.0)),
                500 => self.set("resync", json!(true)),
                501 => self.set("resync", json!(false)),
                700 => self.set("reset", json!(true)),
                730 => self.set("reset", json!(false)),
                _ => {}
            }
            // Offline: frame i is at i / fps and every step is one frame
            // long, the first included (engine.rs).
            let time = frame as f64 / FPS;
            let dt = 1.0 / FPS;
            for (id, op) in &mut ops {
                let sources = if *id == self.clock {
                    vec![]
                } else {
                    vec![Some(self.clock)]
                };
                let mut out = sig.remove(id).unwrap_or_default();
                let mut ctx = SigCtx {
                    inputs: SigInputs::new(&sources, &sig),
                    output: &mut out,
                    params: Params::new(&self.net, &self.reg, &sig, *id)
                        .with_mode(CookMode::Offline { fps: FPS }),
                    time,
                    dt,
                    frame: frame as u64,
                };
                op.cook(&mut ctx).unwrap();
                for c in 0..out.len() {
                    bits.push(out.channel(c)[0].to_bits());
                }
                sig.insert(*id, out);
            }
        }
        bits
    }
}

#[test]
fn the_internal_clock_and_synced_modulators_repeat_exactly_offline() {
    let params = [("bpm", json!(133.0)), ("beats_per_bar", json!(3))];
    let a = Chain::new(&params).run();
    let b = Chain::new(&params).run();
    assert_eq!(a.len(), b.len());
    assert!(a == b, "two offline runs differ");
    // The run is not trivially constant.
    assert!(a.iter().collect::<std::collections::HashSet<_>>().len() > 100);
}

#[test]
fn offline_frame_i_is_a_function_of_i() {
    // No scripted events in the first 100 frames: beat position is
    // (i + 1) / fps * bpm / 60, and the synced LFO is the triangle of it.
    let mut chain = Chain::new(&[("bpm", json!(120.0))]);
    let bits = chain.run();
    let channels_per_frame = 7 + 1 + 3; // clock, lfo, timer
    for frame in [0usize, 1, 30, 59, 99] {
        let row = &bits[frame * channels_per_frame..(frame + 1) * channels_per_frame];
        let f = |i: usize| f32::from_bits(row[i]);
        let beats = (frame + 1) as f64 / FPS * 2.0; // the first frame is a step too
        assert_eq!(f(0), beats.floor() as f32, "frame {frame} beat");
        assert!(
            (f64::from(f(1)) - beats.fract()).abs() < 1e-5,
            "frame {frame} phase"
        );
        // Triangle at (beats * 0.25): peak 1 at phase 0.25... check shape.
        let x = (beats * 0.25).rem_euclid(1.0);
        let want = 1.0 - 4.0 * ((x + 0.25).rem_euclid(1.0) - 0.5).abs();
        assert!((f64::from(f(7)) - want).abs() < 1e-5, "frame {frame} lfo");
    }
}

#[test]
fn midi_source_and_clock_out_are_idle_offline() {
    let params = [
        ("source", json!("midi")),
        ("device", json!("a port that does not exist")),
        ("send_clock", json!(true)),
    ];
    let a = Chain::new(&params).run();
    let b = Chain::new(&params).run();
    assert!(a == b);
    // Never started: the clock sits at beat 0 and the LFO with it.
    assert_eq!(f32::from_bits(a[0]), 0.0);
}
