//! Deterministic fuzzing of IMG cooks: every IMG operator with random
//! wiring and random (valid, often extreme) parameters, cooked for several
//! frames at an odd resolution. Nodes may report errors (missing files,
//! bad shader code); nothing may panic or trip wgpu validation, which
//! panics by default.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::{OpFamily, ParamKind, Registry, WireType};
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
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

/// A random value for a parameter, biased towards range edges.
fn random_value(kind: &ParamKind, rng: &mut Rng) -> Option<Value> {
    Some(match kind {
        ParamKind::Float { min, max } => {
            let lo = min.unwrap_or(-1000.0);
            let hi = max.unwrap_or(1000.0);
            match rng.next(5) {
                0 => json!(lo),
                1 => json!(hi),
                2 => json!(0.0),
                _ => json!(lo + (hi - lo) * rng.unit()),
            }
        }
        ParamKind::Int { min, max } => {
            let lo = min.unwrap_or(-100);
            let hi = max.unwrap_or(100);
            match rng.next(4) {
                0 => json!(lo),
                1 => json!(hi),
                _ => json!(lo + rng.next((hi - lo + 1) as u64) as i64),
            }
        }
        ParamKind::Menu { options } => json!(options[rng.next(options.len() as u64) as usize]),
        ParamKind::Toggle | ParamKind::Pulse => json!(rng.next(2) == 1),
        ParamKind::Colour => json!([rng.unit(), rng.unit(), rng.unit(), rng.unit()]),
        ParamKind::Vec { n } => {
            let v: Vec<f64> = (0..*n).map(|_| (rng.unit() - 0.5) * 4.0).collect();
            json!(v)
        }
        // Strings and paths: occasionally garbage (bad code, missing files).
        ParamKind::String | ParamKind::Path => {
            if rng.next(4) == 0 {
                json!("??? not valid")
            } else {
                return None;
            }
        }
    })
}

#[test]
fn every_img_operator_survives_random_patches() {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let img_types: Vec<String> = reg
        .iter()
        .filter(|s| s.family == OpFamily::Img)
        .map(|s| s.type_id.clone())
        .collect();
    let mut e = Engine::new(
        device,
        queue,
        reg.clone(),
        lib,
        EngineConfig {
            width: 37,
            height: 21,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );

    let seed = std::env::var("ATD_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15u64);
    let mut rng = Rng(seed.max(1));
    let mut t = 0.0;
    // More rounds for local hunting: ATD_FUZZ_ROUNDS=100.
    let rounds = std::env::var("ATD_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    for round in 0..rounds {
        // A fresh patch: every IMG operator once, in a shuffled order, plus
        // an LFO for bindings.
        let ids: Vec<NodeId> = e.network().nodes().map(|n| n.id).collect();
        let removes: Vec<Edit> = ids.into_iter().map(|id| Edit::RemoveNode { id }).collect();
        if !removes.is_empty() {
            e.edit("clear", removes).unwrap();
        }
        let mut order = img_types.clone();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.next(i as u64 + 1) as usize);
        }
        let lfo = e.alloc_id();
        e.edit(
            "lfo",
            [Edit::AddNode {
                node: Node::new(lfo, "sig.lfo", "lfo"),
            }],
        )
        .unwrap();
        let mut placed: Vec<NodeId> = Vec::new();
        for ty in &order {
            let schema = reg.get(ty).unwrap().clone();
            let id = e.alloc_id();
            let name = e.network().unique_name("n");
            e.edit(
                "add",
                [Edit::AddNode {
                    node: Node::new(id, ty.clone(), name),
                }],
            )
            .unwrap();
            // Inputs from earlier nodes (acyclic); feedback inputs from any.
            for socket in schema.inputs.iter().filter(|s| s.wire == WireType::Img) {
                if placed.is_empty() || rng.next(4) == 0 {
                    continue;
                }
                let from = placed[rng.next(placed.len() as u64) as usize];
                // Analyze and Pixel Map output channels, not images: skip
                // them as sources.
                if e.network()
                    .node(from)
                    .is_some_and(|n| n.type_id == "img.analyze" || n.type_id == "img.pixel_map")
                {
                    continue;
                }
                let _ = e.edit(
                    "wire",
                    [Edit::Connect {
                        wire: Wire::new(
                            Endpoint::new(from, "out"),
                            Endpoint::new(id, &socket.name),
                        ),
                    }],
                );
            }
            for p in &schema.params {
                if rng.next(3) == 0 {
                    continue;
                }
                let value = if matches!(p.kind, ParamKind::Float { .. }) && rng.next(6) == 0 {
                    Some(Param::bound(json!(0.0), lfo, "lfo"))
                } else {
                    random_value(&p.kind, &mut rng).map(Param::constant)
                };
                if let Some(v) = value {
                    // Out-of-range values are rejected by validation; fine.
                    let _ = e.edit(
                        "param",
                        [Edit::SetParam {
                            node: id,
                            param: p.name.clone(),
                            value: Some(v),
                        }],
                    );
                }
            }
            placed.push(id);
        }
        let all: Vec<NodeId> = placed.clone();
        for _ in 0..6 {
            e.cook(&all, t)
                .unwrap_or_else(|err| panic!("round {round}: cook failed: {err}"));
            t += 1.0 / 30.0;
        }
        let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
    }
}

/// Operators left out of the live-edit soak: they open devices or send to
/// the network. The SIG fuzz test covers them with safe targets.
const IO: [&str; 9] = [
    "sig.osc_in",
    "sig.osc_out",
    "sig.dmx_out",
    "sig.midi_in",
    "sig.midi_out",
    "sig.audio_in",
    "sig.audio_out",
    "sig.input",
    "sig.timecode_in",
];

/// One long-lived engine, edited at random between frames the way a user
/// patches live: nodes added and deleted, wires made and broken across IMG
/// and SIG, parameters set and bound, undo and redo. Every frame must cook,
/// a deleted node must leave no result behind, and free textures must stay
/// bounded.
#[test]
fn live_edits_between_frames_keep_the_engine_consistent() {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let types: Vec<String> = reg
        .iter()
        .map(|s| s.type_id.clone())
        .filter(|t| !IO.contains(&t.as_str()))
        .collect();
    let mut e = Engine::new(
        device,
        queue,
        reg.clone(),
        lib,
        EngineConfig {
            width: 37,
            height: 21,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    let seed = std::env::var("ATD_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0xA076_1D64_78BD_642Fu64);
    let mut rng = Rng(seed.max(1));
    let steps: usize = std::env::var("ATD_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(400, |r| r * 100);
    let wire_of = |e: &Engine, id: NodeId, output: bool, i: usize| -> Option<(String, WireType)> {
        let node = e.network().node(id)?;
        let schema = reg.get(&node.type_id)?;
        let sockets = if output {
            &schema.outputs
        } else {
            &schema.inputs
        };
        let s = sockets.get(i % sockets.len().max(1))?;
        Some((s.name.clone(), s.wire))
    };
    let mut removed: Vec<NodeId> = Vec::new();
    let mut t = 0.0;
    for step in 0..steps {
        let ids: Vec<NodeId> = e.network().nodes().map(|n| n.id).collect();
        let pick = |rng: &mut Rng| ids[rng.next(ids.len() as u64) as usize];
        let action = if ids.len() > 30 {
            1
        } else if ids.is_empty() {
            0
        } else {
            rng.next(10)
        };
        match action {
            0 | 9 => {
                let ty = types[rng.next(types.len() as u64) as usize].clone();
                let id = e.alloc_id();
                let name = e.network().unique_name("n");
                let mut edits = vec![Edit::AddNode {
                    node: Node::new(id, ty.clone(), name),
                }];
                for p in &reg.get(&ty).unwrap().params {
                    if rng.next(3) == 0
                        && let Some(v) = random_value(&p.kind, &mut rng)
                    {
                        edits.push(Edit::SetParam {
                            node: id,
                            param: p.name.clone(),
                            value: Some(Param::constant(v)),
                        });
                    }
                }
                // Out-of-range values make the batch fail as a whole; retry bare.
                if e.edit("add", edits).is_err() {
                    e.edit(
                        "add",
                        [Edit::AddNode {
                            node: Node::new(id, ty, e.network().unique_name("n")),
                        }],
                    )
                    .unwrap();
                }
            }
            1 => {
                let id = pick(&mut rng);
                e.edit("remove", [Edit::RemoveNode { id }]).unwrap();
                removed.push(id);
            }
            2 | 3 => {
                let (from, to) = (pick(&mut rng), pick(&mut rng));
                let (o, i) = (rng.next(4) as usize, rng.next(4) as usize);
                if let (Some((out, ow)), Some((input, iw))) =
                    (wire_of(&e, from, true, o), wire_of(&e, to, false, i))
                    && ow == iw
                {
                    // Cycles and the like are rejected; fine.
                    let _ = e.edit(
                        "wire",
                        [Edit::Connect {
                            wire: Wire::new(Endpoint::new(from, &out), Endpoint::new(to, &input)),
                        }],
                    );
                }
            }
            4 => {
                let wires: Vec<Wire> = e.network().wires().collect();
                if !wires.is_empty() {
                    let wire = wires[rng.next(wires.len() as u64) as usize].clone();
                    e.edit("unwire", [Edit::Disconnect { wire }]).unwrap();
                }
            }
            5 | 6 => {
                let id = pick(&mut rng);
                let ty = e.network().node(id).unwrap().type_id.clone();
                let params = &reg.get(&ty).unwrap().params;
                if !params.is_empty() {
                    let p = &params[rng.next(params.len() as u64) as usize];
                    let sig = ids.iter().copied().find(|s| {
                        let n = e.network().node(*s).unwrap();
                        reg.get(&n.type_id)
                            .unwrap()
                            .outputs
                            .first()
                            .is_some_and(|o| o.wire == WireType::Sig)
                    });
                    let value = match sig {
                        Some(s)
                            if matches!(p.kind, ParamKind::Float { .. }) && rng.next(3) == 0 =>
                        {
                            Some(Param::bound(json!(0.0), s, "chan1"))
                        }
                        _ => random_value(&p.kind, &mut rng).map(Param::constant),
                    };
                    let _ = e.edit(
                        "param",
                        [Edit::SetParam {
                            node: id,
                            param: p.name.clone(),
                            value,
                        }],
                    );
                }
            }
            7 => {
                e.undo().unwrap();
            }
            _ => {
                e.redo().unwrap();
            }
        }
        let ids: Vec<NodeId> = e.network().nodes().map(|n| n.id).collect();
        e.cook(&ids, t)
            .unwrap_or_else(|err| panic!("step {step}: cook failed: {err}"));
        t += 1.0 / 30.0;
        for id in &removed {
            if e.network().node(*id).is_none() {
                assert!(
                    e.result(*id).is_none(),
                    "step {step}: removed node kept a result"
                );
                assert!(
                    e.channels(*id).is_none(),
                    "step {step}: removed node kept channels"
                );
            }
        }
        assert!(
            e.pool().free_bytes() <= atd_gpu::pool::FREE_BUDGET,
            "step {step}: free textures over budget"
        );
        if step % 50 == 49 {
            let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
        }
    }
    // Deleting everything leaves nothing behind.
    let all: Vec<Edit> = e
        .network()
        .nodes()
        .map(|n| Edit::RemoveNode { id: n.id })
        .collect();
    if !all.is_empty() {
        e.edit("clear", all).unwrap();
    }
    for _ in 0..=atd_gpu::pool::KEEP_FRAMES {
        e.cook(&[], t).unwrap();
        t += 1.0 / 30.0;
    }
    assert_eq!(e.pool().free_count(), 0);
}
