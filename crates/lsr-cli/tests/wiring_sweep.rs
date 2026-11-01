//! Registry-wide wiring sweep: every built-in operator of every family
//! (IMG, SIG, PNT, MSH, MAT, DAT, CMP, AI), built the way the CLI builds
//! its engine, cooked with no inputs and then with a representative source
//! of every wire type on every input (and into a representative consumer
//! of every type from every output). Compatible wires must connect and
//! cook; incompatible ones must be refused at connect time. A second pass
//! cooks seeded random cross-family networks with channel bindings, DAT
//! wires, GEO-to-SIG readbacks and materials into renders, deleting and
//! rewiring between frames.
//!
//! Node errors (a missing file, no AI host, Python not allowed) are fine;
//! panics on any thread and wgpu validation errors are not.
//!
//! Nothing reaches outside the process: the per-operator sweep cooks
//! offline (live inputs idle, sinks silent, `docs/render.md`), AI operators
//! run on a supervisor with no hosts, Sovereign mode stays on, Python stays
//! not allowed, parameters keep their defaults (no file or network target
//! is ever set) and the live-mode random networks leave out every operator
//! in [`LIVE_IO`].
//!
//! Bigger runs locally: `LSR_SWEEP_NETWORKS=200 LSR_SWEEP_SEED=7`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, Once};
use std::time::Instant;

use lsr_core::graph::{Edit, Endpoint, GraphError, Node, NodeId, Param, Wire};
use lsr_core::registry::{OpFamily, ParamKind, Registry, WireType};
use lsr_gpu::engine::{CookMode, Engine, EngineConfig, EngineError, OpLibrary};
use serde_json::json;

const FPS: f64 = 30.0;

/// Every wire type a socket can have. CMP has no built-in socket today:
/// [`every_wire_type_has_a_representative`] fails when one appears, so the
/// sweep grows a representative for it.
const WIRES: [WireType; 6] = [
    WireType::Img,
    WireType::Sig,
    WireType::Geo,
    WireType::Dat,
    WireType::Mat,
    WireType::Cmp,
];

/// Sources wired into every input: (type, output socket). GEO has a point
/// cloud and a mesh, DAT a table and a text, SIG control and noise.
const SOURCES: [(&str, &str); 8] = [
    ("img.noise", "out"),
    ("sig.lfo", "out"),
    ("sig.noise", "out"),
    ("pnt.points", "out"),
    ("msh.box", "out"),
    ("mat.pbr", "out"),
    ("dat.table", "out"),
    ("dat.text", "out"),
];

/// Consumers every output is wired into: (type, input socket).
const CONSUMERS: [(&str, &str); 7] = [
    ("img.level", "in"),
    ("sig.math", "a"),
    ("pnt.transform", "in"),
    ("msh.transform", "in"),
    ("img.render", "geo"),
    ("img.render", "mat"),
    ("dat.select", "in"),
];

/// Operators left out of the live-mode random networks: live they open
/// devices, listen or send on the network, publish shared textures or
/// write files. Offline (the per-operator sweep and the offline networks)
/// they are idle and stay in.
const LIVE_IO: [&str; 26] = [
    "sig.audio_in",
    "sig.audio_out",
    "sig.audio_file_in",
    "sig.midi_in",
    "sig.midi_out",
    "sig.osc_in",
    "sig.osc_out",
    "sig.dmx_out",
    "sig.input",
    "sig.timecode_in",
    "sig.beat",
    "img.share_in",
    "img.share_out",
    "img.pixel_map",
    "img.movie_in",
    "pnt.pixel_out",
    "pnt.sensor_in",
    "pnt.file_in",
    "pnt.splat_in",
    "dat.web_server",
    "dat.web_client",
    "dat.websocket",
    "dat.serial",
    "dat.file_out",
    "msh.file_out",
    "ai.llm_chat",
];

/// Operators the sweep does not cook, with the reason. Empty: offline mode
/// and a host-less AI supervisor keep every built-in operator inside the
/// process.
const SKIPPED: [(&str, &str); 0] = [];

/// Known failures the sweep found but this change does not fix: (type,
/// reason). Each is reported, not asserted.
const KNOWN_FAILURES: [(&str, &str); 0] = [];

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

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The harness check's own panic, which the sweeps ignore.
const ON_PURPOSE: &str = "wiring sweep: a worker panic on purpose";

/// Panics on any thread (worker threads too), recorded by a hook.
fn panics() -> &'static Mutex<Vec<String>> {
    static PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current().name().unwrap_or("?").to_owned();
            if let Ok(mut p) = PANICS.lock() {
                p.push(format!("[{thread}] {info}"));
            }
            default(info);
        }));
    });
    &PANICS
}

/// A device whose validation errors are recorded instead of panicking, so
/// the sweep can name the operator and carry on.
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    errors: Arc<Mutex<Vec<String>>>,
}

fn gpu() -> Option<Gpu> {
    let Ok(ctx) = lsr_gpu::GpuContext::headless() else {
        assert!(
            !std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return None;
    };
    let errors: Arc<Mutex<Vec<String>>> = Arc::default();
    let slot = errors.clone();
    ctx.device
        .on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            if let Ok(mut s) = slot.lock() {
                s.push(e.to_string());
            }
        }));
    Some(Gpu {
        device: ctx.device,
        queue: ctx.queue,
        errors,
    })
}

/// A supervisor with no hosts: AI operators get "no AI host" node errors
/// and nothing is spawned or connected.
fn no_hosts() -> &'static lsr_ai::supervisor::Supervisor {
    static SUP: std::sync::OnceLock<&'static lsr_ai::supervisor::Supervisor> =
        std::sync::OnceLock::new();
    SUP.get_or_init(|| Box::leak(Box::new(lsr_ai::supervisor::Supervisor::new())))
}

/// The registry and library as the CLI builds them
/// (`lsr-cli/src/project.rs` `library_with`).
fn library() -> (Registry, OpLibrary) {
    let mut registry = Registry::default();
    lsr_ops_img::register(&mut registry);
    lsr_ops_sig::register(&mut registry);
    lsr_ops_pnt::register(&mut registry);
    lsr_ops_msh::register(&mut registry);
    lsr_ops_mat::register(&mut registry);
    lsr_ops_dat::register(&mut registry);
    lsr_ops_cmp::register(&mut registry);
    lsr_ops_ai::register(&mut registry);
    let mut library = OpLibrary::default();
    lsr_ops_img::library(&mut library);
    lsr_ops_sig::library(&mut library);
    lsr_ops_pnt::library(&mut library);
    lsr_ops_mat::library(&mut library);
    lsr_ops_dat::library(&mut library);
    lsr_py::script::library(&mut library);
    lsr_ops_msh::library(&mut library);
    lsr_ops_cmp::library(&mut library);
    lsr_ops_ai::library_with(&mut library, no_hosts());
    (registry, library)
}

fn engine(gpu: &Gpu, mode: CookMode) -> Engine {
    let (registry, library) = library();
    let mut e = Engine::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        registry,
        library,
        EngineConfig {
            width: 37,
            height: 21,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    e.set_mode(mode);
    e
}

fn outside_stays_closed() {
    assert!(
        lsr_ai::egress::sovereign(),
        "Sovereign mode must stay on (no internet egress)"
    );
    assert_eq!(
        lsr_py::allow::generation(),
        0,
        "nothing may be put on the Python allow-list"
    );
}

/// Cooks every node for `frames` frames, then waits for the GPU.
fn cook(e: &mut Engine, frame: &mut u64, frames: u32) -> Result<(), String> {
    for _ in 0..frames {
        let all: Vec<NodeId> = e.network().nodes().map(|n| n.id).collect();
        e.set_frame(*frame);
        e.cook(&all, *frame as f64 / FPS)
            .map_err(|err| format!("frame {frame}: cook failed: {err}"))?;
        *frame += 1;
    }
    let _ = e.device().poll(wgpu::PollType::wait_indefinitely());
    Ok(())
}

fn add(e: &mut Engine, type_id: &str) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name("n");
    e.edit(
        "add",
        [Edit::AddNode {
            node: Node::new(id, type_id, name),
        }],
    )
    .unwrap_or_else(|err| panic!("adding {type_id}: {err}"));
    id
}

fn clear(e: &mut Engine) {
    let removes: Vec<Edit> = e
        .network()
        .nodes()
        .map(|n| Edit::RemoveNode { id: n.id })
        .collect();
    if !removes.is_empty() {
        e.edit("clear", removes).unwrap();
    }
}

fn connect(
    e: &mut Engine,
    from: NodeId,
    out: &str,
    to: NodeId,
    input: &str,
) -> Result<(), EngineError> {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, out), Endpoint::new(to, input)),
        }],
    )
}

fn wire_of(reg: &Registry, type_id: &str, socket: &str, output: bool) -> WireType {
    let s = reg.get(type_id).expect("registered");
    let sockets = if output { &s.outputs } else { &s.inputs };
    sockets
        .iter()
        .find(|x| x.name == socket)
        .unwrap_or_else(|| panic!("{type_id} has no socket {socket}"))
        .wire
}

#[derive(Default)]
struct Coverage {
    operators: usize,
    inputs: usize,
    outputs: usize,
    accepted: usize,
    refused: usize,
    pairs: std::collections::BTreeSet<(String, String)>,
    frames: u64,
    node_errors: usize,
}

/// One operator: bare, then every source into every input, every output
/// into every consumer, everything wired at once, then deleted mid-run.
fn sweep_one(
    e: &mut Engine,
    reg: &Registry,
    type_id: &str,
    frame: &mut u64,
    cov: &mut Coverage,
) -> Result<(), String> {
    clear(e);
    let schema = reg.get(type_id).unwrap().clone();
    let node = add(e, type_id);
    cook(e, frame, 3)?;
    if e.error(node).is_some() {
        cov.node_errors += 1;
    }
    let sources: Vec<(NodeId, &str, WireType)> = SOURCES
        .iter()
        .map(|(t, s)| (add(e, t), *s, wire_of(reg, t, s, true)))
        .collect();
    for socket in &schema.inputs {
        cov.inputs += 1;
        for &(src, out, wire) in &sources {
            let result = connect(e, src, out, node, &socket.name);
            cov.pairs
                .insert((format!("{wire:?}"), format!("{:?}", socket.wire)));
            if wire == socket.wire {
                result.map_err(|err| {
                    format!(
                        "{wire:?} into {:?} input `{}` refused: {err}",
                        socket.wire, socket.name
                    )
                })?;
                cov.accepted += 1;
                cook(e, frame, 2)?;
            } else {
                match result {
                    Err(EngineError::Graph(GraphError::WireTypeMismatch { .. })) => {
                        cov.refused += 1;
                    }
                    other => {
                        return Err(format!(
                            "{wire:?} into {:?} input `{}`: expected a type mismatch, got {other:?}",
                            socket.wire, socket.name
                        ));
                    }
                }
            }
        }
        // Back to unwired, then the next socket.
        let wires: Vec<Wire> = e.network().wires().filter(|w| w.to.node == node).collect();
        if !wires.is_empty() {
            e.edit(
                "unwire",
                wires.into_iter().map(|wire| Edit::Disconnect { wire }),
            )
            .map_err(|err| err.to_string())?;
        }
        cook(e, frame, 1)?;
    }
    // Every output into every consumer.
    let consumers: Vec<(NodeId, &str, WireType)> = CONSUMERS
        .iter()
        .map(|(t, s)| (add(e, t), *s, wire_of(reg, t, s, false)))
        .collect();
    for socket in &schema.outputs {
        cov.outputs += 1;
        for &(dst, input, wire) in &consumers {
            let result = connect(e, node, &socket.name, dst, input);
            cov.pairs
                .insert((format!("{:?}", socket.wire), format!("{wire:?}")));
            if wire == socket.wire {
                result.map_err(|err| {
                    format!(
                        "output `{}` into {wire:?} consumer refused: {err}",
                        socket.name
                    )
                })?;
                cov.accepted += 1;
                cook(e, frame, 2)?;
            } else {
                match result {
                    Err(EngineError::Graph(GraphError::WireTypeMismatch { .. })) => {
                        cov.refused += 1;
                    }
                    other => {
                        return Err(format!(
                            "output `{}` ({:?}) into {wire:?} consumer: expected a type mismatch, got {other:?}",
                            socket.name, socket.wire
                        ));
                    }
                }
            }
        }
    }
    // Every input wired at once (first matching source), then cooked.
    for socket in &schema.inputs {
        if let Some(&(src, out, _)) = sources.iter().find(|s| s.2 == socket.wire) {
            connect(e, src, out, node, &socket.name).map_err(|err| err.to_string())?;
        }
    }
    cook(e, frame, 3)?;
    // Deleted while wired both ways: downstream keeps cooking, nothing of
    // the node is left behind.
    e.edit("remove", [Edit::RemoveNode { id: node }])
        .map_err(|err| err.to_string())?;
    cook(e, frame, 2)?;
    if e.result(node).is_some() || e.channels(node).is_some() || e.geometry(node).is_some() {
        return Err("removed node kept a result".into());
    }
    // Undo brings it back with its wires; it cooks again.
    e.undo().map_err(|err| err.to_string())?;
    cook(e, frame, 2)?;
    cov.operators += 1;
    Ok(())
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".into())
}

/// The sweep's net catches what it claims to: a wgpu validation error and
/// a panic on a worker thread are both recorded.
#[test]
fn the_sweep_records_validation_errors_and_worker_panics() {
    let panics = panics();
    let Some(gpu) = gpu() else { return };
    // A mappable storage buffer is invalid without MAPPABLE_PRIMARY_BUFFERS.
    let _ = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("invalid on purpose"),
        size: 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    assert!(!gpu.errors.lock().unwrap().is_empty());
    let _ = std::thread::spawn(|| panic!("{ON_PURPOSE}")).join();
    assert!(
        panics
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains(ON_PURPOSE))
    );
}

#[test]
fn every_wire_type_has_a_representative() {
    let (reg, _) = library();
    for wire in WIRES {
        let used = reg
            .iter()
            .any(|s| s.inputs.iter().chain(&s.outputs).any(|x| x.wire == wire));
        let has_source = SOURCES
            .iter()
            .any(|(t, s)| wire_of(&reg, t, s, true) == wire);
        let has_consumer = CONSUMERS
            .iter()
            .any(|(t, s)| wire_of(&reg, t, s, false) == wire);
        assert!(
            !used || (has_source && has_consumer),
            "{wire:?} sockets exist: add a source and a consumer for it to the sweep"
        );
    }
}

/// The per-operator sweep over `families`' operators.
fn sweep(families: &[OpFamily]) {
    let panics = panics();
    let Some(gpu) = gpu() else { return };
    outside_stays_closed();
    let started = Instant::now();
    let mode = CookMode::Offline { fps: FPS };
    let (reg, _) = library();
    let mut e = engine(&gpu, mode);
    let mut cov = Coverage::default();
    let mut frame = 0u64;
    let mut failures: Vec<String> = Vec::new();
    let mut slowest: Vec<(f64, String)> = Vec::new();
    let types: Vec<String> = reg
        .iter()
        .filter(|s| families.contains(&s.family))
        .map(|s| s.type_id.clone())
        .collect();
    for type_id in &types {
        if SKIPPED.iter().any(|(t, _)| t == type_id) {
            continue;
        }
        if std::env::var_os("LSR_SWEEP_VERBOSE").is_some() {
            eprintln!("sweep: {type_id}");
        }
        let t0 = Instant::now();
        let before = panics.lock().unwrap().len();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            sweep_one(&mut e, &reg, type_id, &mut frame, &mut cov)
        }));
        slowest.push((t0.elapsed().as_secs_f64(), type_id.clone()));
        let mut problems: Vec<String> = Vec::new();
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(why)) => problems.push(why),
            Err(p) => {
                problems.push(format!("panicked: {}", panic_text(&*p)));
                // The engine may be mid-frame: start over.
                e = engine(&gpu, mode);
            }
        }
        problems.extend(
            panics.lock().unwrap()[before..]
                .iter()
                .filter(|p| !p.contains(ON_PURPOSE))
                .map(|p| format!("panic: {p}")),
        );
        problems.extend(
            gpu.errors
                .lock()
                .unwrap()
                .drain(..)
                .map(|v| format!("wgpu: {v}")),
        );
        let known = KNOWN_FAILURES.iter().find(|(t, _)| t == type_id);
        for p in problems {
            match known {
                Some((_, why)) => eprintln!("known failure {type_id} ({why}): {p}"),
                None => failures.push(format!("{type_id}: {p}")),
            }
        }
    }
    cov.frames = frame;
    outside_stays_closed();
    let skipped = types
        .iter()
        .filter(|t| SKIPPED.iter().any(|(s, _)| s == t))
        .count();
    slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
    eprintln!(
        "wiring sweep {families:?}: {} operators ({} skipped), {} inputs, {} outputs, {} wires accepted, {} refused, {} wire-type pairs, {} frames, {} operators with a node error bare, {:.1}s; slowest: {:?}",
        cov.operators,
        skipped,
        cov.inputs,
        cov.outputs,
        cov.accepted,
        cov.refused,
        cov.pairs.len(),
        cov.frames,
        cov.node_errors,
        started.elapsed().as_secs_f64(),
        &slowest[..slowest.len().min(5)],
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(cov.operators + skipped, types.len());
    // Every wire type in use was tried into each of these inputs and out
    // of each of these outputs.
    let name = |w: WireType| format!("{w:?}");
    let used: std::collections::BTreeSet<String> = reg
        .iter()
        .flat_map(|s| s.inputs.iter().chain(&s.outputs).map(|x| name(x.wire)))
        .collect();
    for s in reg.iter().filter(|s| families.contains(&s.family)) {
        for a in &used {
            for i in &s.inputs {
                let pair = (a.clone(), name(i.wire));
                assert!(cov.pairs.contains(&pair), "{pair:?} never tried");
            }
            for o in &s.outputs {
                let pair = (name(o.wire), a.clone());
                assert!(cov.pairs.contains(&pair), "{pair:?} never tried");
            }
        }
    }
}

/// Images, signals, data, materials, compositing and AI.
const SWEEP_A: [OpFamily; 6] = [
    OpFamily::Img,
    OpFamily::Sig,
    OpFamily::Dat,
    OpFamily::Mat,
    OpFamily::Cmp,
    OpFamily::Ai,
];
/// Points and meshes, a test of their own (run in parallel with the other):
/// MSH cooks wait for their workers.
const SWEEP_B: [OpFamily; 2] = [OpFamily::Pnt, OpFamily::Msh];

#[test]
fn every_img_sig_dat_mat_cmp_ai_operator_takes_every_compatible_wire_and_refuses_the_rest() {
    sweep(&SWEEP_A);
}

#[test]
fn every_pnt_msh_operator_takes_every_compatible_wire_and_refuses_the_rest() {
    sweep(&SWEEP_B);
}

#[test]
fn the_two_sweeps_cover_every_family() {
    for f in OpFamily::ALL {
        assert!(
            SWEEP_A.contains(&f) ^ SWEEP_B.contains(&f),
            "{f:?} must be in exactly one sweep"
        );
    }
}

/// A random value for a numeric, toggle or menu parameter; strings and
/// paths keep their defaults (no file or network target is ever set).
fn random_value(kind: &ParamKind, rng: &mut Rng) -> Option<serde_json::Value> {
    Some(match kind {
        ParamKind::Float { min, max } => {
            let lo = min.unwrap_or(-100.0);
            let hi = max.unwrap_or(100.0);
            match rng.next(4) {
                0 => json!(lo),
                1 => json!(hi),
                _ => json!(lo + (hi - lo) * rng.unit()),
            }
        }
        ParamKind::Int { min, max } => {
            // Counts and sizes: stay modest so a network cooks quickly.
            let lo = min.unwrap_or(-10).max(-10);
            let hi = max.unwrap_or(10).min(lo + 16);
            json!(lo + rng.next((hi - lo + 1) as u64) as i64)
        }
        ParamKind::Menu { options } => json!(options[rng.next(options.len() as u64) as usize]),
        ParamKind::Toggle | ParamKind::Pulse => json!(rng.next(2) == 1),
        ParamKind::Colour => json!([rng.unit(), rng.unit(), rng.unit(), rng.unit()]),
        ParamKind::Vec { n } => json!(
            (0..*n)
                .map(|_| (rng.unit() - 0.5) * 4.0)
                .collect::<Vec<_>>()
        ),
        ParamKind::String | ParamKind::Path => return None,
    })
}

/// Live, a menu can point an operator at a device or the network (Pixel
/// Map's protocol, a timecode source); those operators are in [`LIVE_IO`].
/// Offline everything is idle, so menus are free.
fn sig_outputs(e: &Engine, reg: &Registry) -> Vec<NodeId> {
    e.network()
        .nodes()
        .filter(|n| {
            reg.get(&n.type_id)
                .is_some_and(|s| s.outputs.iter().any(|o| o.wire == WireType::Sig))
        })
        .map(|n| n.id)
        .collect()
}

/// Wires a random earlier node into each input of `id` (most of them), so
/// the network stays acyclic; feedback inputs may take any node.
fn wire_randomly(e: &mut Engine, reg: &Registry, id: NodeId, rng: &mut Rng) {
    let ty = e.network().node(id).unwrap().type_id.clone();
    let inputs = reg.get(&ty).unwrap().inputs.clone();
    let nodes: Vec<(NodeId, String)> = e
        .network()
        .nodes()
        .filter(|n| n.id != id)
        .map(|n| (n.id, n.type_id.clone()))
        .collect();
    for socket in inputs {
        if rng.next(4) == 0 {
            continue;
        }
        let candidates: Vec<(NodeId, String)> = nodes
            .iter()
            .flat_map(|(n, t)| {
                reg.get(t)
                    .unwrap()
                    .outputs
                    .iter()
                    .filter(|o| o.wire == socket.wire)
                    .map(move |o| (*n, o.name.clone()))
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let (from, out) = &candidates[rng.next(candidates.len() as u64) as usize];
        // Cycles are refused; fine.
        let _ = connect(e, *from, out, id, &socket.name);
    }
}

/// Binds a random follow-able parameter of a random node to a channel a
/// SIG output actually has.
fn bind_randomly(e: &mut Engine, reg: &Registry, rng: &mut Rng) -> bool {
    let sigs = sig_outputs(e, reg);
    let ids: Vec<NodeId> = e.network().nodes().map(|n| n.id).collect();
    if sigs.is_empty() || ids.is_empty() {
        return false;
    }
    let node = ids[rng.next(ids.len() as u64) as usize];
    let ty = e.network().node(node).unwrap().type_id.clone();
    let params: Vec<_> = reg
        .get(&ty)
        .unwrap()
        .params
        .iter()
        .filter(|p| p.kind.can_follow())
        .cloned()
        .collect();
    if params.is_empty() {
        return false;
    }
    let p = &params[rng.next(params.len() as u64) as usize];
    let src = sigs[rng.next(sigs.len() as u64) as usize];
    let channel = e
        .channels(src)
        .filter(|c| !c.is_empty())
        .map_or("chan1".to_owned(), |c| {
            c.name(rng.next(c.len() as u64) as usize).to_owned()
        });
    // Cycles (binding upstream to downstream) are refused; fine.
    e.edit(
        "bind",
        [Edit::SetParam {
            node,
            param: p.name.clone(),
            value: Some(Param::bound(p.default.clone(), src, channel)),
        }],
    )
    .is_ok()
}

#[derive(Default)]
struct NetStats {
    networks: usize,
    nodes: usize,
    wires: usize,
    bindings: usize,
    frames: u64,
}

fn one_network(
    e: &mut Engine,
    reg: &Registry,
    types: &[String],
    live: bool,
    rng: &mut Rng,
    frame: &mut u64,
    stats: &mut NetStats,
) -> Result<(), String> {
    clear(e);
    // A core that always exercises the cross-family paths: points and a
    // mesh into a render with a material, points read back to channels,
    // DAT wired, SIG for bindings.
    let core = [
        "sig.lfo",
        "img.noise",
        "pnt.points",
        "msh.box",
        "mat.pbr",
        "img.render",
        "pnt.to_channels",
        "dat.table",
        "dat.select",
    ];
    let count = 24 + rng.next(16) as usize;
    let pool: Vec<&String> = types
        .iter()
        .filter(|t| !live || !LIVE_IO.contains(&t.as_str()))
        .collect();
    let mut order: Vec<String> = core.iter().map(|s| (*s).to_owned()).collect();
    for _ in 0..count {
        order.push(pool[rng.next(pool.len() as u64) as usize].clone());
    }
    let mut ids: Vec<NodeId> = Vec::new();
    for ty in &order {
        let id = add(e, ty);
        ids.push(id);
        let params = reg.get(ty).unwrap().params.clone();
        for p in &params {
            if rng.next(3) != 0 {
                continue;
            }
            if let Some(v) = random_value(&p.kind, rng) {
                // Out-of-range values are refused; fine.
                let _ = e.edit(
                    "param",
                    [Edit::SetParam {
                        node: id,
                        param: p.name.clone(),
                        value: Some(Param::constant(v)),
                    }],
                );
            }
        }
        wire_randomly(e, reg, id, rng);
    }
    // The core's cross-family wires, whatever the random wiring chose.
    for (from, to, input) in [
        (ids[2], ids[6], "in"),  // points read back to channels
        (ids[3], ids[5], "geo"), // a mesh into the render
        (ids[4], ids[5], "mat"), // with a material
        (ids[2], ids[5], "geo2"),
        (ids[7], ids[8], "in"), // a DAT wire
    ] {
        connect(e, from, "out", to, input).map_err(|err| err.to_string())?;
    }
    cook(e, frame, 3)?;
    // A parameter downstream follows the readback's channels.
    let readback = e
        .channels(ids[6])
        .filter(|c| !c.is_empty())
        .map(|c| c.name(0).to_owned());
    if let Some(channel) = readback {
        e.edit(
            "bind",
            [Edit::SetParam {
                node: ids[1],
                param: "z".into(),
                value: Some(Param::bound(json!(0.0), ids[6], channel)),
            }],
        )
        .map_err(|err| err.to_string())?;
        stats.bindings += 1;
    }
    for _ in 0..8 {
        if bind_randomly(e, reg, rng) {
            stats.bindings += 1;
        }
    }
    cook(e, frame, 2)?;
    // Mid-run: delete some nodes, add and wire others, rewire, undo.
    for step in 0..6 {
        let ids: Vec<NodeId> = e.network().nodes().map(|n| n.id).collect();
        match rng.next(4) {
            0 if !ids.is_empty() => {
                let id = ids[rng.next(ids.len() as u64) as usize];
                e.edit("remove", [Edit::RemoveNode { id }])
                    .map_err(|err| err.to_string())?;
            }
            1 => {
                let ty = pool[rng.next(pool.len() as u64) as usize].clone();
                let id = add(e, &ty);
                wire_randomly(e, reg, id, rng);
            }
            2 if !ids.is_empty() => {
                let id = ids[rng.next(ids.len() as u64) as usize];
                wire_randomly(e, reg, id, rng);
            }
            _ => {
                e.undo().map_err(|err| err.to_string())?;
            }
        }
        cook(e, frame, 1 + (step % 2))?;
    }
    stats.networks += 1;
    stats.nodes += e.network().nodes().count();
    stats.wires += e.network().wires().count();
    Ok(())
}

#[test]
fn random_cross_family_networks_cook_and_survive_live_edits() {
    let panics = panics();
    let Some(gpu) = gpu() else { return };
    outside_stays_closed();
    let started = Instant::now();
    let (reg, _) = library();
    let types: Vec<String> = reg
        .iter()
        .map(|s| s.type_id.clone())
        .filter(|t| !SKIPPED.iter().any(|(s, _)| s == t))
        .collect();
    let networks: usize = env_or("LSR_SWEEP_NETWORKS", 12);
    let seed: u64 = env_or("LSR_SWEEP_SEED", 0x2545_F491_4F6C_DD1D);
    let mut rng = Rng(seed.max(1));
    let mut stats = NetStats::default();
    let mut frame = 0u64;
    let mut failures: Vec<String> = Vec::new();
    for n in 0..networks {
        // Every other network live (workers run free, results arrive late).
        let live = n % 2 == 1;
        let mode = if live {
            CookMode::Live
        } else {
            CookMode::Offline { fps: FPS }
        };
        let mut e = engine(&gpu, mode);
        let before = panics.lock().unwrap().len();
        let t0 = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            one_network(&mut e, &reg, &types, live, &mut rng, &mut frame, &mut stats)
        }));
        if std::env::var_os("LSR_SWEEP_VERBOSE").is_some() {
            eprintln!("network {n}: {:.1}s", t0.elapsed().as_secs_f64());
        }
        let mut problems: Vec<String> = Vec::new();
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(why)) => problems.push(why),
            Err(p) => problems.push(format!("panicked: {}", panic_text(&*p))),
        }
        problems.extend(
            panics.lock().unwrap()[before..]
                .iter()
                .filter(|p| !p.contains(ON_PURPOSE))
                .map(|p| format!("panic: {p}")),
        );
        problems.extend(
            gpu.errors
                .lock()
                .unwrap()
                .drain(..)
                .map(|v| format!("wgpu: {v}")),
        );
        if !problems.is_empty() {
            let types: Vec<&str> = e.network().nodes().map(|n| n.type_id.as_str()).collect();
            for p in problems {
                failures.push(format!(
                    "network {n} ({}, seed {seed:#x}): {p}\n  nodes: {types:?}",
                    if live { "live" } else { "offline" }
                ));
            }
        }
    }
    stats.frames = frame;
    outside_stays_closed();
    eprintln!(
        "random networks: {} cooked, {} nodes and {} wires at the end, {} bindings, {} frames, {:.1}s",
        stats.networks,
        stats.nodes,
        stats.wires,
        stats.bindings,
        stats.frames,
        started.elapsed().as_secs_f64()
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(stats.networks, networks);
}
