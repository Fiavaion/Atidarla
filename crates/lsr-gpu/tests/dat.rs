//! DAT routing end to end: values flow along DAT wires, a DAT node cooks
//! only when its inputs change, SIG operators read and write DAT values,
//! and a DAT operator polling a worker cooks again until it is done.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use lsr_gpu::engine::{Dat, DatCtx, DatOp, Engine, EngineConfig, OpLibrary, SigCtx, SigOp, Table};
use serde_json::json;

fn socket(name: &str, wire: WireType) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire,
        optional: true,
        feedback: false,
    }
}

fn schema(
    type_id: &str,
    family: OpFamily,
    inputs: Vec<SocketSchema>,
    outputs: Vec<SocketSchema>,
    time_dependent: bool,
) -> OperatorSchema {
    OperatorSchema {
        type_id: type_id.into(),
        family,
        label: type_id.into(),
        summary: "test operator".into(),
        version: 1,
        params: vec![ParamSchema::string("text", "Text", "hello")],
        inputs,
        outputs,
        capabilities: vec![],
        cook_budget_us: 100,
        time_dependent,
        sink: false,
    }
}

/// Outputs its `text` parameter (time-dependent when registered so).
struct Source(Arc<AtomicUsize>);
impl DatOp for Source {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ctx.output.replace(Dat::text(ctx.params.str("text")));
        Ok(())
    }
}

/// Upper-cases its input; counts its cooks.
struct Upper(Arc<AtomicUsize>);
impl DatOp for Upper {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let text = ctx.inputs.get(0).map(|d| d.as_text().to_uppercase());
        ctx.output.replace(Dat::text(text.unwrap_or_default()));
        Ok(())
    }
}

/// Pretends to wait for a worker: done on its third cook.
struct Slow(u32);
impl DatOp for Slow {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        self.0 += 1;
        if self.0 < 3 {
            ctx.cook_again = true;
        } else {
            ctx.output
                .replace(Dat::table(Table::from_rows([["done", "3"]])));
        }
        Ok(())
    }

    fn status(&self) -> Option<&str> {
        Some("slow")
    }
}

/// A SIG operator with a DAT input and a DAT output: the input's length as
/// a channel, and the input reversed.
struct Measure;
impl SigOp for Measure {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let text = ctx
            .dat
            .inputs
            .get(0)
            .map(|d| d.as_text().into_owned())
            .unwrap_or_default();
        ctx.output.writer().push("len", text.len() as f32);
        if let Some(out) = ctx.dat.output.as_deref_mut() {
            out.replace(Dat::text(text.chars().rev().collect::<String>()));
        }
        Ok(())
    }
}

struct Rig {
    engine: Engine,
    sources: Arc<AtomicUsize>,
    uppers: Arc<AtomicUsize>,
}

fn rig() -> Option<Rig> {
    let instance = lsr_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let dat = || vec![socket("out", WireType::Dat)];
    let mut reg = Registry::default();
    for (id, td) in [("dat.source", false), ("dat.ticker", true)] {
        reg.register(schema(id, OpFamily::Dat, vec![], dat(), td))
            .unwrap();
    }
    reg.register(schema(
        "dat.upper",
        OpFamily::Dat,
        vec![socket("in", WireType::Dat)],
        dat(),
        false,
    ))
    .unwrap();
    reg.register(schema("dat.slow", OpFamily::Dat, vec![], dat(), false))
        .unwrap();
    reg.register(schema("dat.missing", OpFamily::Dat, vec![], dat(), false))
        .unwrap();
    reg.register(schema(
        "sig.measure",
        OpFamily::Sig,
        vec![socket("in", WireType::Dat)],
        vec![socket("out", WireType::Sig), socket("text", WireType::Dat)],
        false,
    ))
    .unwrap();
    let sources = Arc::new(AtomicUsize::new(0));
    let uppers = Arc::new(AtomicUsize::new(0));
    let mut lib = OpLibrary::default();
    for id in ["dat.source", "dat.ticker"] {
        let n = sources.clone();
        lib.add_dat(id, move || Box::new(Source(n.clone())));
    }
    let n = uppers.clone();
    lib.add_dat("dat.upper", move || Box::new(Upper(n.clone())));
    lib.add_dat("dat.slow", || Box::new(Slow(0)));
    lib.add_sig("sig.measure", || Box::new(Measure));
    let config = EngineConfig {
        width: 4,
        height: 4,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Rig {
        engine: Engine::new(device, queue, reg, lib, config),
        sources,
        uppers,
    })
}

fn add(e: &mut Engine, type_id: &str, name: &str) -> NodeId {
    let id = e.alloc_id();
    e.edit(
        "add",
        [Edit::AddNode {
            node: Node::new(id, type_id, name),
        }],
    )
    .unwrap();
    id
}

fn wire(e: &mut Engine, from: NodeId, out: &str, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, out), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

fn set_text(e: &mut Engine, node: NodeId, text: &str) {
    e.edit(
        "text",
        [Edit::SetParam {
            node,
            param: "text".into(),
            value: Some(Param::constant(json!(text))),
        }],
    )
    .unwrap();
}

fn text(e: &Engine, node: NodeId) -> String {
    e.dat(node).expect("a DAT value").as_text().into_owned()
}

macro_rules! rig_or_skip {
    () => {
        match rig() {
            Some(r) => r,
            None => {
                eprintln!("skipping: no GPU adapter");
                return;
            }
        }
    };
}

#[test]
fn dat_values_flow_along_wires_and_cache() {
    let Rig {
        engine: mut e,
        uppers,
        ..
    } = rig_or_skip!();
    let src = add(&mut e, "dat.source", "src");
    let up = add(&mut e, "dat.upper", "up");
    wire(&mut e, src, "out", up, "in");
    assert_eq!(e.cook(&[up], 0.0).unwrap(), 2);
    assert_eq!(text(&e, up), "HELLO");
    assert_eq!(e.cook(&[up], 0.1).unwrap(), 0, "nothing changed");
    set_text(&mut e, src, "again");
    e.cook(&[up], 0.2).unwrap();
    assert_eq!(text(&e, up), "AGAIN");
    assert_eq!(uppers.load(Ordering::SeqCst), 2);
}

#[test]
fn consumers_of_an_unchanged_value_do_not_cook() {
    let Rig {
        engine: mut e,
        sources,
        uppers,
    } = rig_or_skip!();
    let tick = add(&mut e, "dat.ticker", "tick");
    let up = add(&mut e, "dat.upper", "up");
    wire(&mut e, tick, "out", up, "in");
    for f in 0..5 {
        e.cook(&[up], f64::from(f) / 60.0).unwrap();
    }
    // The ticker cooks every frame; its value never changes, so Upper
    // cooked once.
    assert_eq!(sources.load(Ordering::SeqCst), 5);
    assert_eq!(uppers.load(Ordering::SeqCst), 1);
    let before = e.dat(up).unwrap().clone();
    set_text(&mut e, tick, "new");
    e.cook(&[up], 1.0).unwrap();
    assert_eq!(text(&e, up), "NEW");
    assert!(!Dat::ptr_eq(&before, e.dat(up).unwrap()));
    assert_eq!(uppers.load(Ordering::SeqCst), 2);
    // Disconnecting the input is a change too.
    e.edit(
        "cut",
        [Edit::Disconnect {
            wire: Wire::new(Endpoint::new(tick, "out"), Endpoint::new(up, "in")),
        }],
    )
    .unwrap();
    e.cook(&[up], 1.1).unwrap();
    assert_eq!(text(&e, up), "");
}

#[test]
fn sig_operators_read_and_write_dat() {
    let Rig { engine: mut e, .. } = rig_or_skip!();
    let src = add(&mut e, "dat.source", "src");
    let m = add(&mut e, "sig.measure", "m");
    let up = add(&mut e, "dat.upper", "up");
    wire(&mut e, src, "out", m, "in");
    wire(&mut e, m, "text", up, "in");
    e.cook(&[up, m], 0.0).unwrap();
    assert_eq!(e.channels(m).unwrap().value("len"), Some(5.0));
    assert_eq!(text(&e, m), "olleh");
    assert_eq!(text(&e, up), "OLLEH");
}

#[test]
fn polling_operators_cook_again_until_done() {
    let Rig { engine: mut e, .. } = rig_or_skip!();
    let slow = add(&mut e, "dat.slow", "slow");
    e.cook(&[slow], 0.0).unwrap();
    assert!(e.pending());
    assert_eq!(e.status(slow), Some("slow"));
    e.cook(&[slow], 0.1).unwrap();
    e.cook(&[slow], 0.2).unwrap();
    assert!(!e.pending());
    assert_eq!(e.dat(slow).unwrap().cell(0, 0), Some("done"));
    assert_eq!(e.cook(&[slow], 0.3).unwrap(), 0);
}

#[test]
fn missing_implementations_and_removed_nodes() {
    let Rig { engine: mut e, .. } = rig_or_skip!();
    let missing = add(&mut e, "dat.missing", "missing");
    e.cook(&[missing], 0.0).unwrap();
    assert!(e.error(missing).unwrap().contains("no DAT implementation"));
    let src = add(&mut e, "dat.source", "src");
    e.cook(&[src], 0.1).unwrap();
    assert!(e.dat(src).is_some());
    e.edit("remove", [Edit::RemoveNode { id: src }]).unwrap();
    assert!(e.dat(src).is_none());
}
