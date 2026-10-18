//! GEO wires end to end (design: docs/design/geometry-attributes.md §3.2):
//! caching, copy on write, the in-place hand-over to a sole consumer,
//! simulation state through GEO feedback, IMG operators reading geometry,
//! and steady-state pooling. Test-only operators; skips without a GPU
//! unless `ATD_REQUIRE_GPU` is set.

use std::sync::{Arc, Mutex};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, CookMode, Engine, EngineConfig, GeoCtx, GeoOp, GpuOp, OpLibrary};
use atd_gpu::geo::kernels::{Deformer, Packer, Wrote};
use atd_gpu::geo::{Attribute, Residency, readback};
use serde_json::json;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()
}

fn socket(name: &str, wire: WireType, feedback: bool) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire,
        optional: true,
        feedback,
    }
}

fn schema(
    type_id: &str,
    inputs: Vec<SocketSchema>,
    output: WireType,
    params: Vec<ParamSchema>,
    time_dependent: bool,
) -> OperatorSchema {
    let family = if output == WireType::Img {
        OpFamily::Img
    } else {
        OpFamily::Pnt
    };
    OperatorSchema {
        type_id: type_id.into(),
        family,
        label: type_id.into(),
        summary: "test operator".into(),
        version: 1,
        params,
        inputs,
        outputs: vec![socket("out", output, false)],
        capabilities: vec![],
        cook_budget_us: 1000,
        time_dependent,
        sink: false,
    }
}

/// `count` points at P = (i + value, 0, 0), made on the CPU and uploaded.
struct Gen;
impl GeoOp for Gen {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let n = ctx.params.i64("count").max(0) as u32;
        let v = ctx.params.f32("value");
        let p: Vec<[f32; 3]> = (0..n).map(|i| [i as f32 + v, 0.0, 0.0]).collect();
        let p = Attribute::from_vec3("P", &p);
        p.ensure_gpu(ctx.queue, ctx.buffers);
        let header = ctx.cpu_header(n);
        ctx.output.clear();
        ctx.output.set_points(n, Some(header));
        ctx.output.set_point(p);
        // A CPU-only attribute rides along untouched.
        ctx.output
            .set_point(Attribute::from_u32("id", &(0..n).collect::<Vec<_>>()));
        Ok(())
    }
}

/// Shared record of how each offset node wrote, by node name.
type Log = Arc<Mutex<Vec<(String, Wrote)>>>;

/// P.x += amount, through a [`Deformer`].
struct Offset {
    deform: Deformer,
    log: Log,
}
impl GeoOp for Offset {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        let mut p = Packer::<16>::new();
        p.f32(ctx.params.f32("amount"));
        let wrote = self.deform.apply(ctx, &mut g, "P", p.bytes())?;
        let name = ctx.params.str("tag").to_owned();
        self.log.lock().unwrap().push((name, wrote));
        *ctx.output = g;
        Ok(())
    }
}

/// A solver step: `init` on the first frame (or without `prev`), else
/// `prev` with P.x + 1. `prev` is a feedback input.
struct Step {
    deform: Deformer,
}
impl GeoOp for Step {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let prev_ok = ctx.input(1).is_some_and(|g| g.points() > 0);
        if !prev_ok {
            *ctx.output = ctx.take_input(0).unwrap_or_default();
            return Ok(());
        }
        let mut g = ctx.take_input(1).expect("checked");
        let mut p = Packer::<16>::new();
        p.f32(1.0);
        self.deform.apply(ctx, &mut g, "P", p.bytes())?;
        *ctx.output = g;
        Ok(())
    }
}

/// An IMG operator reading geometry: fills its output with red = points/255.
struct CountImg;
impl GpuOp for CountImg {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let n = ctx.geo_inputs.get(0).map_or(0, |g| g.points());
        let view = ctx.output.create_view(&Default::default());
        ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(n) / 255.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        Ok(())
    }
}

const ADD_X: &str = "struct Params { amount: f32, pad0: f32, pad1: f32, pad2: f32 };";
const ADD_X_BODY: &str = "return value + vec4f(params.amount, 0.0, 0.0, 0.0);";

fn setup(device: wgpu::Device, queue: wgpu::Queue) -> (Engine, Log) {
    let geo = WireType::Geo;
    let mut reg = Registry::default();
    let count = ParamSchema::int("count", "Count", 4, Some(0), None);
    let value = ParamSchema::float("value", "Value", 0.0, None, None);
    let amount = ParamSchema::float("amount", "Amount", 1.0, None, None);
    let tag = ParamSchema::string("tag", "Tag", "");
    reg.register(schema("pnt.gen", vec![], geo, vec![count, value], false))
        .unwrap();
    for (id, live) in [("pnt.offset", false), ("pnt.offset_live", true)] {
        reg.register(schema(
            id,
            vec![socket("in", geo, false)],
            geo,
            vec![amount.clone(), tag.clone()],
            live,
        ))
        .unwrap();
    }
    reg.register(schema(
        "pnt.step",
        vec![socket("init", geo, false), socket("prev", geo, true)],
        geo,
        vec![],
        false,
    ))
    .unwrap();
    reg.register(schema(
        "img.count",
        vec![socket("geo", geo, false)],
        WireType::Img,
        vec![],
        false,
    ))
    .unwrap();
    let log: Log = Arc::default();
    let mut lib = OpLibrary::default();
    lib.add_geo("pnt.gen", |_| Box::new(Gen));
    for id in ["pnt.offset", "pnt.offset_live"] {
        let log = log.clone();
        lib.add_geo(id, move |d| {
            Box::new(Offset {
                deform: Deformer::new(d, "test-offset", ADD_X, ADD_X_BODY, 16),
                log: log.clone(),
            })
        });
    }
    lib.add_geo("pnt.step", |d| {
        Box::new(Step {
            deform: Deformer::new(d, "test-step", ADD_X, ADD_X_BODY, 16),
        })
    });
    lib.add("img.count", |_, _| Box::new(CountImg));
    let config = EngineConfig {
        width: 8,
        height: 8,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    (Engine::new(device, queue, reg, lib, config), log)
}

fn node(
    e: &mut Engine,
    edits: &mut Vec<Edit>,
    ty: &str,
    params: &[(&str, serde_json::Value)],
) -> NodeId {
    let id = e.alloc_id();
    edits.push(Edit::AddNode {
        node: Node::new(id, ty, format!("n{}", id.0)),
    });
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    id
}

fn wire(edits: &mut Vec<Edit>, from: NodeId, to: NodeId, socket: &str) {
    edits.push(Edit::Connect {
        wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
    });
}

/// The x of every point of `node`'s geometry.
fn xs(e: &Engine, node: NodeId) -> Vec<f32> {
    let g = e.geometry(node).expect("geometry");
    let p = g.point("P").expect("P");
    let bytes = readback::read_blocking(e.device(), e.queue(), e.buffer_pool(), p).unwrap();
    bytes
        .chunks_exact(16)
        .map(|c| f32::from_le_bytes(c[..4].try_into().unwrap()))
        .collect()
}

fn writes(log: &Log, tag: &str) -> Vec<Wrote> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == tag)
        .map(|(_, w)| *w)
        .collect()
}

#[test]
fn geo_results_cook_once_and_read_back() {
    let Some((device, queue)) = gpu() else { return };
    let (mut e, log) = setup(device, queue);
    let mut edits = Vec::new();
    let g = node(
        &mut e,
        &mut edits,
        "pnt.gen",
        &[("count", json!(5)), ("value", json!(1.0))],
    );
    let o = node(
        &mut e,
        &mut edits,
        "pnt.offset",
        &[("amount", json!(10.0)), ("tag", json!("o"))],
    );
    wire(&mut edits, g, o, "in");
    e.edit("build", edits).unwrap();
    assert_eq!(e.cook(&[o], 0.0).unwrap(), 2);
    assert_eq!(e.error(o), None);
    let geo = e.geometry(o).unwrap();
    assert_eq!(geo.points(), 5);
    // P was written on the GPU; `id` is still the generator's CPU copy.
    assert_eq!(geo.point("P").unwrap().residency(), Residency::Gpu);
    assert_eq!(geo.point("id").unwrap().residency(), Residency::Cpu);
    assert_eq!(geo.residency(), Some(Residency::Gpu));
    assert_eq!(xs(&e, o), [11.0, 12.0, 13.0, 14.0, 15.0]);
    // Reading back added a CPU copy: P is now on both sides.
    let geo = e.geometry(o).unwrap();
    assert_eq!(geo.point("P").unwrap().residency(), Residency::Both);
    assert_eq!(e.cook(&[o], 0.1).unwrap(), 0, "cached");
    let geo = e.geometry(o).unwrap();
    assert!(
        geo.point("id")
            .unwrap()
            .shares(e.geometry(g).unwrap().point("id").unwrap()),
        "untouched attributes are shared, not copied"
    );
    // The static generator's value cannot be handed over: copy on write.
    assert_eq!(writes(&log, "o"), [Wrote::Copy]);
    assert_eq!(xs(&e, g), [1.0, 2.0, 3.0, 4.0, 5.0], "the source is intact");
}

#[test]
fn a_sole_consumer_writes_in_place_and_a_viewed_source_is_copied() {
    let Some((device, queue)) = gpu() else { return };
    let (mut e, log) = setup(device, queue);
    let mut edits = Vec::new();
    let g = node(&mut e, &mut edits, "pnt.gen", &[("count", json!(3))]);
    let a = node(
        &mut e,
        &mut edits,
        "pnt.offset_live",
        &[("amount", json!(1.0)), ("tag", json!("a"))],
    );
    let b = node(
        &mut e,
        &mut edits,
        "pnt.offset_live",
        &[("amount", json!(100.0)), ("tag", json!("b"))],
    );
    wire(&mut edits, g, a, "in");
    wire(&mut edits, a, b, "in");
    e.edit("build", edits).unwrap();
    for i in 0..5 {
        e.cook(&[b], f64::from(i) / 60.0).unwrap();
        assert_eq!(
            xs(&e, b),
            [101.0, 102.0, 103.0],
            "frame {i}: not accumulating"
        );
    }
    // First frame: nothing is continuous yet... a is time-dependent, so b
    // takes a's value from the first frame on and writes it in place; a
    // copies from the static generator every frame.
    assert!(writes(&log, "a").iter().all(|w| *w == Wrote::Copy));
    assert_eq!(writes(&log, "b"), [Wrote::InPlace; 5]);
    assert!(e.geometry(a).is_none(), "a's value moved to b");
    assert_eq!(xs(&e, g), [0.0, 1.0, 2.0], "the generator is intact");

    // Viewing a as well: b must not take it, and copies instead.
    log.lock().unwrap().clear();
    for i in 5..8 {
        e.cook(&[a, b], f64::from(i) / 60.0).unwrap();
        assert_eq!(xs(&e, a), [1.0, 2.0, 3.0]);
        assert_eq!(xs(&e, b), [101.0, 102.0, 103.0]);
    }
    assert_eq!(writes(&log, "b"), [Wrote::Copy; 3]);
}

#[test]
fn a_second_reader_prevents_the_hand_over() {
    let Some((device, queue)) = gpu() else { return };
    let (mut e, log) = setup(device, queue);
    let mut edits = Vec::new();
    let g = node(&mut e, &mut edits, "pnt.gen", &[("count", json!(2))]);
    let a = node(
        &mut e,
        &mut edits,
        "pnt.offset_live",
        &[("amount", json!(1.0)), ("tag", json!("a"))],
    );
    let b = node(
        &mut e,
        &mut edits,
        "pnt.offset",
        &[("amount", json!(10.0)), ("tag", json!("b"))],
    );
    let c = node(
        &mut e,
        &mut edits,
        "pnt.offset",
        &[("amount", json!(20.0)), ("tag", json!("c"))],
    );
    wire(&mut edits, g, a, "in");
    wire(&mut edits, a, b, "in");
    wire(&mut edits, a, c, "in");
    e.edit("build", edits).unwrap();
    for i in 0..3 {
        e.cook(&[b, c], f64::from(i) / 60.0).unwrap();
    }
    assert_eq!(xs(&e, b), [11.0, 12.0]);
    assert_eq!(xs(&e, c), [21.0, 22.0]);
    assert!(
        writes(&log, "b")
            .iter()
            .chain(&writes(&log, "c"))
            .all(|w| *w == Wrote::Copy)
    );
}

#[test]
fn simulation_state_travels_on_a_feedback_wire() {
    let Some((device, queue)) = gpu() else { return };
    let (mut e, _) = setup(device, queue);
    let mut edits = Vec::new();
    let g = node(
        &mut e,
        &mut edits,
        "pnt.gen",
        &[("count", json!(2)), ("value", json!(5.0))],
    );
    // A solver wired into its own feedback input...
    let s = node(&mut e, &mut edits, "pnt.step", &[]);
    wire(&mut edits, g, s, "init");
    wire(&mut edits, s, s, "prev");
    // ... and one whose loop goes through another node (+0.5 per frame).
    let s2 = node(&mut e, &mut edits, "pnt.step", &[]);
    let o = node(
        &mut e,
        &mut edits,
        "pnt.offset",
        &[("amount", json!(0.5)), ("tag", json!("o"))],
    );
    wire(&mut edits, g, s2, "init");
    wire(&mut edits, s2, o, "in");
    wire(&mut edits, o, s2, "prev");
    e.edit("build", edits).unwrap();
    let created_after = |e: &mut Engine, frames: u32| {
        for i in 0..frames {
            e.cook(&[s, o], f64::from(i) / 60.0).unwrap();
        }
        e.buffer_pool().created()
    };
    created_after(&mut e, 1);
    assert_eq!(xs(&e, s), [5.0, 6.0], "frame 0 passes init");
    assert_eq!(xs(&e, o), [5.5, 6.5]);
    created_after(&mut e, 9);
    assert_eq!(xs(&e, s), [14.0, 15.0], "nine steps of +1");
    assert_eq!(xs(&e, o), [5.0 + 9.0 * 1.5 + 0.5, 6.0 + 9.0 * 1.5 + 0.5]);
    // The loop ping-pongs between pooled generations: no new buffers (the
    // readbacks above made their staging buffers already).
    let warm = e.buffer_pool().created();
    let later = created_after(&mut e, 30);
    assert_eq!(later, warm, "steady state creates no buffers");
}

#[test]
fn img_operators_read_geometry_inputs() {
    let Some((device, queue)) = gpu() else { return };
    let (mut e, _) = setup(device, queue);
    let mut edits = Vec::new();
    let g = node(&mut e, &mut edits, "pnt.gen", &[("count", json!(51))]);
    let img = node(&mut e, &mut edits, "img.count", &[]);
    wire(&mut edits, g, img, "geo");
    e.edit("build", edits).unwrap();
    e.cook(&[img], 0.0).unwrap();
    let px =
        atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(img).unwrap()).unwrap();
    assert_eq!(&px[..4], &[51, 0, 0, 255]);
}

#[test]
fn missing_geo_implementations_and_offline_mode() {
    let Some((device, queue)) = gpu() else { return };
    let (mut e, _) = setup(device, queue);
    e.extend(|r, _| {
        r.register(schema("pnt.nothing", vec![], WireType::Geo, vec![], false))
            .unwrap()
    });
    let mut edits = Vec::new();
    let n = node(&mut e, &mut edits, "pnt.nothing", &[]);
    e.edit("build", edits).unwrap();
    e.cook(&[n], 0.0).unwrap();
    assert!(e.error(n).unwrap().contains("no GEO implementation"));

    // Offline, the same chain gives the same values (new buffers are
    // cleared, so nothing depends on the pool's history).
    let run = |mode| {
        let Some((device, queue)) = gpu() else {
            unreachable!()
        };
        let (mut e, _) = setup(device, queue);
        e.set_mode(mode);
        let mut edits = Vec::new();
        let g = node(&mut e, &mut edits, "pnt.gen", &[("count", json!(4))]);
        let s = node(&mut e, &mut edits, "pnt.step", &[]);
        wire(&mut edits, g, s, "init");
        wire(&mut edits, s, s, "prev");
        e.edit("build", edits).unwrap();
        for i in 0..6 {
            e.set_frame(i);
            e.cook(&[s], i as f64 / 60.0).unwrap();
        }
        xs(&e, s)
    };
    let offline = run(CookMode::Offline { fps: 60.0 });
    assert_eq!(offline, [5.0, 6.0, 7.0, 8.0]);
    assert_eq!(run(CookMode::Live), offline);
}
