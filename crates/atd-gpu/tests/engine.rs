//! Engine end to end on whatever GPU is available (lavapipe locally, WARP on
//! Windows CI, Metal on macOS CI), with test-only operators.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::{
    OpFamily, OperatorSchema, ParamKind, ParamSchema, Registry, SocketSchema, WireType,
};
use atd_gpu::engine::{CookCtx, Engine, EngineConfig, GpuOp, OpLibrary};
use atd_gpu::fullscreen::FullscreenPass;
use atd_interop::readback::read_rgba8;
use serde_json::json;

fn no_gpu() {
    let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(
        !required,
        "ATD_REQUIRE_GPU is set but there is no GPU adapter"
    );
    eprintln!("skipping: no GPU adapter");
}

fn socket(name: &str, feedback: bool) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback,
    }
}

fn schema(
    type_id: &str,
    inputs: Vec<SocketSchema>,
    params: Vec<ParamSchema>,
    time_dependent: bool,
) -> OperatorSchema {
    OperatorSchema {
        type_id: type_id.into(),
        family: OpFamily::Img,
        label: type_id.into(),
        summary: "test operator".into(),
        version: 1,
        params,
        inputs,
        outputs: vec![socket("out", false)],
        capabilities: vec![],
        cook_budget_us: 1000,
        time_dependent,
    }
}

/// Fills the output with `colour`.
struct Fill(FullscreenPass);
impl GpuOp for Fill {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let c = ctx.params.vec4("colour");
        self.0
            .draw(ctx.device, ctx.queue, ctx.encoder, &[], &[c], ctx.output);
        Ok(())
    }
}

/// out = 1 - in (RGB), alpha kept.
struct Invert(FullscreenPass);
impl GpuOp for Invert {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let input = ctx.input_or_black(0).clone();
        self.0.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&input],
            &[],
            ctx.output,
        );
        Ok(())
    }
}

/// out = in * 0.5 + prev * 0.5, where `prev` is a feedback input.
struct Accumulate(FullscreenPass);
impl GpuOp for Accumulate {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let a = ctx.input_or_black(0).clone();
        let b = ctx.input_or_black(1).clone();
        self.0.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&a, &b],
            &[],
            ctx.output,
        );
        Ok(())
    }
}

/// Always fails, to test error reporting.
struct Broken;
impl GpuOp for Broken {
    fn cook(&mut self, _: &mut CookCtx<'_>) -> Result<(), String> {
        Err("deliberate failure".into())
    }
}

const W: u32 = 8;
const H: u32 = 4;

fn engine() -> Option<Engine> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;

    let mut reg = Registry::default();
    let colour = ParamSchema {
        name: "colour".into(),
        label: "Colour".into(),
        kind: ParamKind::Colour,
        default: json!([0.0, 0.0, 0.0, 1.0]),
        help: String::new(),
    };
    reg.register(schema("img.fill", vec![], vec![colour], false))
        .unwrap();
    reg.register(schema(
        "img.invert",
        vec![socket("in", false)],
        vec![],
        false,
    ))
    .unwrap();
    reg.register(schema(
        "img.accumulate",
        vec![socket("in", false), socket("prev", true)],
        vec![],
        false,
    ))
    .unwrap();
    reg.register(schema("img.broken", vec![], vec![], false))
        .unwrap();
    reg.register(schema("img.unimplemented", vec![], vec![], false))
        .unwrap();

    let mut lib = OpLibrary::default();
    lib.add("img.fill", |d, f| {
        Box::new(Fill(FullscreenPass::new(
            d,
            "fill",
            f,
            0,
            "",
            "return params.v[0];",
        )))
    });
    lib.add("img.invert", |d, f| {
        Box::new(Invert(FullscreenPass::new(
            d,
            "invert",
            f,
            1,
            "",
            "let c = textureSample(in0, samp, uv); return vec4f(1.0 - c.rgb, c.a);",
        )))
    });
    lib.add("img.accumulate", |d, f| {
        Box::new(Accumulate(FullscreenPass::new(
            d,
            "accumulate",
            f,
            2,
            "",
            "return textureSample(in0, samp, uv) * 0.5 + textureSample(in1, samp, uv) * 0.5;",
        )))
    });
    lib.add("img.broken", |_, _| Box::new(Broken));

    let config = EngineConfig {
        width: W,
        height: H,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
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

fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

fn set_colour(e: &mut Engine, node: NodeId, c: [f64; 4]) {
    e.edit(
        "colour",
        [Edit::SetParam {
            node,
            param: "colour".into(),
            value: Some(Param::constant(json!(c))),
        }],
    )
    .unwrap();
}

/// The first pixel of a node's result.
fn pixel(e: &Engine, node: NodeId) -> [u8; 4] {
    let tex = e.result(node).expect("node has a result");
    let data = read_rgba8(e.device(), e.queue(), tex).unwrap();
    assert!(data.chunks(4).all(|p| p == &data[..4]), "uniform image");
    data[..4].try_into().unwrap()
}

#[test]
fn fill_invert_chain_cooks_and_caches() {
    let Some(mut e) = engine() else {
        return no_gpu();
    };
    let fill = add(&mut e, "img.fill", "fill1");
    let inv = add(&mut e, "img.invert", "invert1");
    wire(&mut e, fill, inv, "in");
    set_colour(&mut e, fill, [1.0, 0.0, 0.2, 1.0]);

    e.cook(&[inv], 0.0).unwrap();
    assert_eq!(pixel(&e, fill), [255, 0, 51, 255]);
    assert_eq!(pixel(&e, inv), [0, 255, 204, 255]);
    assert_eq!(e.stat(inv).unwrap().cooks, 1);

    // Nothing changed: nothing recooks, no new textures.
    let created = e.pool().created();
    e.cook(&[inv], 1.0).unwrap();
    assert_eq!(e.stat(inv).unwrap().cooks, 1);
    assert_eq!(e.pool().created(), created);

    // A parameter edit recooks the chain and reuses pooled textures.
    set_colour(&mut e, fill, [0.0, 0.0, 0.0, 1.0]);
    e.cook(&[inv], 2.0).unwrap();
    assert_eq!(pixel(&e, inv), [255, 255, 255, 255]);
    for t in 3..10 {
        set_colour(&mut e, fill, [t as f64 / 10.0, 0.0, 0.0, 1.0]);
        e.cook(&[inv], t as f64).unwrap();
    }
    assert!(
        e.pool().created() <= created + 2,
        "steady state reuses pooled textures"
    );

    // Undo restores the previous colour.
    e.undo().unwrap();
    e.cook(&[inv], 10.0).unwrap();
    assert_eq!(pixel(&e, fill), [204, 0, 0, 255]);
}

#[test]
fn feedback_accumulates_across_frames() {
    let Some(mut e) = engine() else {
        return no_gpu();
    };
    let fill = add(&mut e, "img.fill", "fill1");
    let acc = add(&mut e, "img.accumulate", "acc1");
    wire(&mut e, fill, acc, "in");
    wire(&mut e, acc, acc, "prev");
    set_colour(&mut e, fill, [1.0, 1.0, 1.0, 1.0]);

    // acc(n) = 0.5 * 255 + 0.5 * acc(n-1), starting from black: 128, 191, 223, …
    let mut last = 0u8;
    for frame in 0..4 {
        e.cook(&[acc], frame as f64).unwrap();
        let r = pixel(&e, acc)[0];
        assert!(r > last, "frame {frame}: {r} should exceed {last}");
        last = r;
    }
    assert!(last >= 230, "converges towards white, got {last}");
}

#[test]
fn errors_are_reported_and_downstream_sees_black() {
    let Some(mut e) = engine() else {
        return no_gpu();
    };
    let broken = add(&mut e, "img.broken", "broken1");
    let missing = add(&mut e, "img.unimplemented", "missing1");
    let inv = add(&mut e, "img.invert", "invert1");
    wire(&mut e, broken, inv, "in");
    e.cook(&[inv, missing], 0.0).unwrap();
    assert_eq!(e.error(broken), Some("deliberate failure"));
    assert!(e.error(missing).unwrap().contains("no GPU implementation"));
    assert!(e.result(broken).is_none());
    // invert(black transparent) = white, alpha 0.
    assert_eq!(pixel(&e, inv), [255, 255, 255, 0]);
}

#[test]
fn removing_a_node_frees_its_state() {
    let Some(mut e) = engine() else {
        return no_gpu();
    };
    let fill = add(&mut e, "img.fill", "fill1");
    e.cook(&[fill], 0.0).unwrap();
    assert!(e.result(fill).is_some());
    e.edit("remove", [Edit::RemoveNode { id: fill }]).unwrap();
    assert!(e.result(fill).is_none());
    assert!(e.stat(fill).is_none());
    // Undo brings the node back; it recooks on demand.
    e.undo().unwrap();
    e.cook(&[fill], 1.0).unwrap();
    assert_eq!(pixel(&e, fill), [0, 0, 0, 255]);
}
