//! Per-point attribute operators of M6: Attribute Noise (fractal gradient or
//! curl noise on any attribute), Attribute from Channels (SIG samples or
//! channels looked up per point) and Falloff Field (a weight from a sphere,
//! box or plane, the effector falloff of a deformer stack).
//!
//! All write any float or integer attribute through the runtime type
//! descriptor (`kit::typed_access`), in place when the engine hands the
//! geometry over, and dispatch from the input's header, so GPU-counted
//! geometry works too.

use lsr_core::geo::{AttrType, names};
use lsr_core::registry::{OperatorSchema, ParamSchema, SocketSchema, WireType};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params};
use lsr_gpu::geo::Geometry;
use lsr_gpu::geo::kernels::{Bind, Packer};

use crate::kit::{self, Missing, PointKernel, TYPES, desc, menu_index, typed_access, unit, vec3};

fn dummy(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 256,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    })
}

fn attr_name<'a>(p: &Params<'a>, param: &str) -> Result<&'a str, String> {
    let name = p.str(param);
    if lsr_core::geo::valid_name(name) {
        Ok(name)
    } else {
        Err(format!("`{name}` is not a valid attribute name"))
    }
}

/// The input to build on, or nothing to do (the output set).
fn input(ctx: &mut GeoCtx<'_>) -> Option<Geometry> {
    let Some(g) = ctx.take_input(0) else {
        ctx.output.clear();
        return None;
    };
    if g.points() == 0 {
        *ctx.output = g;
        return None;
    }
    Some(g)
}

/// The type of the attribute written: an existing one keeps its type,
/// a new one gets `ty`.
fn target_type(g: &Geometry, name: &str, ty: AttrType) -> AttrType {
    g.point(name).map_or(ty, |a| a.ty())
}

const COMBINE: &[&str] = &["set", "add", "multiply", "max", "min"];

/// WGSL: `fn combine(old, v, op, amount)`, from a [`COMBINE`] index.
const COMBINE_WGSL: &str = "fn combine(old: vec4f, v: vec4f, op: u32, amount: f32) -> vec4f {
    var r = v;
    switch op {
        case 1u: { r = old + v; }
        case 2u: { r = old * v; }
        case 3u: { r = max(old, v); }
        case 4u: { r = min(old, v); }
        default: {}
    }
    return mix(old, r, amount);
}
";

fn combine_params(default: &str) -> [ParamSchema; 2] {
    [
        ParamSchema::menu("operation", "Operation", COMBINE, default).help(
            "How the result meets the attribute's value: set (replace), add, multiply, max or min.",
        ),
        ParamSchema::float("amount", "Amount", 1.0, None, None)
            .help("Blends from the attribute's value (0) to the combined result (1)."),
    ]
}

// ------------------------------------------------------------ Attribute Noise

const NOISES: &[&str] = &["gradient", "curl"];

pub fn noise_schema() -> OperatorSchema {
    let mut params = vec![
        ParamSchema::string("name", "Name", "Cd").help("The attribute changed (created at zero, with Type, when missing)."),
        ParamSchema::menu("type", "Type", TYPES, "vec4").help("The type of a new attribute (an existing one keeps its type)."),
        ParamSchema::menu("noise", "Noise", NOISES, "gradient").help("gradient: smooth fractal noise, a different field per component; curl: a divergence-free swirl (three components; the fourth gets gradient noise)."),
        ParamSchema::string("position", "Position", "P").help("The attribute the noise is evaluated at (any vector, usually `P`; `uv` for a texture-like pattern)."),
        ParamSchema::float("amplitude", "Amplitude", 0.5, None, None).help("Scale of the noise (about −1 to 1 before scaling)."),
        ParamSchema::float("centre", "Centre", 0.0, None, None).help("Added to the noise: 0.5 with amplitude 0.5 gives values from 0 to 1."),
        ParamSchema::float("frequency", "Frequency", 2.0, Some(0.0), None).help("Noise cells per unit."),
        ParamSchema::int("octaves", "Octaves", 3, Some(1), Some(8)).help("Layers of detail, each at twice the frequency."),
        ParamSchema::float("roughness", "Roughness", 0.5, Some(0.0), Some(1.0)).help("Amplitude of each octave relative to the one before."),
        ParamSchema::vec("offset", "Offset", &[0.0, 0.0, 0.0]).help("Moves the noise field."),
        ParamSchema::float("evolution", "Evolution", 0.0, None, None).help("Moves the field through a fourth direction so the pattern changes rather than slides; drive it from time."),
        ParamSchema::int("seed", "Seed", 0, Some(0), None).help("Selects a different field."),
    ];
    params.extend(combine_params("add"));
    crate::pnt(
        "attribute_noise",
        "Attribute Noise",
        "Fractal noise on any point attribute: gradient noise (a different field per component) or curl noise, evaluated at `P` (or another vector attribute), scaled, centred and combined with the attribute's value (set, add, multiply, max, min). Animate it with Evolution.",
        &[("in", false)],
        params,
        80,
    )
}

const NOISE: &str = "struct Params {
    offset: vec4f,
    // amplitude, centre, frequency, roughness
    a: vec4f,
    d: vec4u,
    dp: vec4u,
    noise: u32,
    octaves: u32,
    seed: u32,
    op: u32,
    amount: f32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};
@group(0) @binding(2) var<storage, read_write> A: array<u32>;
@group(0) @binding(3) var<storage, read> X: array<u32>;
";

const NOISE_BODY: &str = "fn fbm(q: vec3f, seed: u32) -> f32 {
    var s = 0.0;
    var f = 1.0;
    var a = 1.0;
    var norm = 0.0;
    for (var o = 0u; o < params.octaves; o = o + 1u) {
        s = s + lsr_gnoise(q * f, seed + o * 17u).x * a;
        norm = norm + a;
        f = f * 2.0;
        a = a * params.a.w;
    }
    return s / max(norm, 1e-6);
}
fn point(i: u32) {
    let q = ld_X(i).xyz * params.a.z + params.offset.xyz;
    var n = vec4f(
        fbm(q, params.seed),
        fbm(q, params.seed + 1000u),
        fbm(q, params.seed + 2000u),
        fbm(q, params.seed + 3000u),
    );
    if (params.noise == 1u) {
        n = vec4f(lsr_fbm_curl(q, params.seed, params.octaves) * 0.5, n.w);
    }
    let v = n * params.a.x + vec4f(params.a.y);
    st_A(i, combine(ld_A(i), v, params.op, params.amount));
}
";

pub struct AttributeNoise {
    kernel: PointKernel,
}

pub fn create_noise(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(AttributeNoise {
        kernel: PointKernel::new(
            device,
            "pnt.attribute_noise",
            &format!(
                "{NOISE}{}{}{COMBINE_WGSL}{NOISE_BODY}",
                typed_access("A", "params.d", false),
                typed_access("X", "params.dp", true)
            ),
            &[Bind::ReadWrite, Bind::Read],
            112,
        ),
    })
}

impl GeoOp for AttributeNoise {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let name = attr_name(&p, "name")?;
        let position = attr_name(&p, "position")?;
        if position == name {
            return Err(format!(
                "`{name}` cannot be both the position and the result; copy it first (Attribute Math, set)"
            ));
        }
        let (x, x_ty) = kit::readable_any(ctx, &g, position)?;
        let ty = target_type(&g, name, kit::type_named(p.str("type")));
        let hdr = kit::header_of(&g)?;
        let a = kit::writable(ctx, &mut g, name, ty, Missing::Zero)?;
        let o = vec3(&p, "offset");
        let ev = p.f32("evolution");
        let mut u = Packer::<112>::new();
        u.vec4([o[0] + ev * 0.31, o[1] - ev * 0.17, o[2] + ev, 0.0])
            .vec4([
                p.f32("amplitude"),
                p.f32("centre"),
                p.f32("frequency").max(0.0),
                p.f32("roughness").clamp(0.0, 1.0),
            ]);
        for w in desc(ty).into_iter().chain(desc(x_ty)) {
            u.u32(w);
        }
        u.u32(menu_index(NOISES, p.str("noise")))
            .u32(p.i64("octaves").clamp(1, 8) as u32)
            .u32(kit::seed(&p))
            .u32(menu_index(COMBINE, p.str("operation")))
            .f32(p.f32("amount"));
        self.kernel.run(ctx, &hdr, &[&a, &x], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}

// --------------------------------------------------- Attribute from Channels

const CHANNEL_LOOKUPS: &[&str] = &["index", "attribute", "channels"];

pub fn channels_schema() -> OperatorSchema {
    let mut params = vec![
        ParamSchema::string("name", "Name", "pscale").help("The attribute written (created at zero, with Type, when missing)."),
        ParamSchema::menu("type", "Type", TYPES, "float").help("The type of a new attribute (an existing one keeps its type)."),
        ParamSchema::menu("lookup", "Lookup", CHANNEL_LOOKUPS, "index").help(
            "index: point i reads sample i of the channels (a spectrum or a trail across the points); attribute: the float attribute Attribute (0 to 1) picks the sample position, interpolated; channels: point i reads the last sample of channel i (one value per channel, such as a bank of LFOs).",
        ),
        ParamSchema::string("attribute", "Attribute", "u").help("attribute: the float attribute (0 at the first sample, 1 at the last)."),
        ParamSchema::int("first", "First Channel", 0, Some(0), None).help("index, attribute: the channel the first component reads; the next components read the next channels (the last channel repeats)."),
        ParamSchema::toggle("wrap", "Wrap", false).help("index, channels: wraps past the last sample or channel instead of holding it."),
        ParamSchema::float("scale", "Scale", 1.0, None, None).help("Multiplies the values read."),
    ];
    params.extend(combine_params("set"));
    OperatorSchema {
        inputs: vec![
            SocketSchema {
                name: "in".into(),
                wire: WireType::Geo,
                optional: true,
                feedback: false,
            },
            SocketSchema {
                name: "channels".into(),
                wire: WireType::Sig,
                optional: true,
                feedback: false,
            },
        ],
        ..crate::pnt(
            "attribute_from_channels",
            "Attribute from Channels",
            "Writes SIG values into a point attribute: point i reads sample i (a spectrum or a history across the points), a sample picked by a 0-1 float attribute (interpolated), or the last value of channel i (a bank of LFOs, one per point). A vector attribute's components read consecutive channels. Combined with the attribute's value (set, add, multiply, max, min). The channels are uploaded each frame they change.",
            &[("in", false)],
            params,
            60,
        )
    }
}

const CHANNELS: &str = "struct Params {
    d: vec4u,
    da: vec4u,
    // channels, samples, first, lookup
    s: vec4u,
    // wrap, op, unused, unused
    m: vec4u,
    // scale, amount, unused, unused
    k: vec4f,
};
@group(0) @binding(2) var<storage, read_write> A: array<u32>;
@group(0) @binding(3) var<storage, read> U: array<u32>;
@group(0) @binding(4) var<storage, read> S: array<f32>;
";

const CHANNELS_BODY: &str = "fn sample(c: u32, k: u32) -> f32 {
    return S[min(c, params.s.x - 1u) * params.s.y + k];
}
fn index(i: u32, n: u32) -> u32 {
    if (params.m.x == 1u) { return i % n; }
    return min(i, n - 1u);
}
fn point(i: u32) {
    let comps = params.d.x;
    var v = vec4f(0.0);
    for (var k = 0u; k < 4u; k = k + 1u) {
        if (k >= comps) { break; }
        let c = params.s.z + k;
        var x = 0.0;
        switch params.s.w {
            case 1u: {
                let t = clamp(ld_U(i).x, 0.0, 1.0) * f32(params.s.y - 1u);
                let a = u32(floor(t));
                let b = min(a + 1u, params.s.y - 1u);
                x = mix(sample(c, a), sample(c, b), t - f32(a));
            }
            case 2u: {
                x = S[index(i, params.s.x) * params.s.y + params.s.y - 1u];
            }
            default: { x = sample(c, index(i, params.s.y)); }
        }
        if (k == 0u) { v.x = x; }
        if (k == 1u) { v.y = x; }
        if (k == 2u) { v.z = x; }
        if (k == 3u) { v.w = x; }
    }
    st_A(i, combine(ld_A(i), v * params.k.x, params.m.y, params.k.y));
}
";

pub struct AttributeFromChannels {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
    /// The channels as floats, channel after channel (grows, never shrinks).
    samples: Option<wgpu::Buffer>,
    scratch: Vec<u8>,
}

pub fn create_channels(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(AttributeFromChannels {
        kernel: PointKernel::new(
            device,
            "pnt.attribute_from_channels",
            &format!(
                "{CHANNELS}{}{}{COMBINE_WGSL}{CHANNELS_BODY}",
                typed_access("A", "params.d", false),
                typed_access("U", "params.da", true)
            ),
            &[ReadWrite, Read, Read],
            80,
        ),
        dummy: dummy(device, "pnt.attribute_from_channels.unused"),
        samples: None,
        scratch: Vec::new(),
    })
}

impl GeoOp for AttributeFromChannels {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let Some(ch) = ctx
            .sig_inputs
            .get(1)
            .filter(|c| !c.is_empty() && c.samples() > 0)
        else {
            *ctx.output = g;
            return Ok(());
        };
        let (channels, samples) = (ch.len(), ch.samples());
        self.scratch.clear();
        for c in 0..channels {
            for v in ch.channel(c) {
                self.scratch.extend_from_slice(&v.to_le_bytes());
            }
        }
        let bytes = self.scratch.len() as u64;
        if self.samples.as_ref().is_none_or(|b| b.size() < bytes) {
            self.samples = Some(ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pnt.attribute_from_channels.samples"),
                size: bytes.next_power_of_two().max(256),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buf = self.samples.clone().expect("made above");
        ctx.queue.write_buffer(&buf, 0, &self.scratch);
        let p = ctx.params;
        let name = attr_name(&p, "name")?;
        let lookup = menu_index(CHANNEL_LOOKUPS, p.str("lookup"));
        let (u, u_ty) = if lookup == 1 {
            let a = attr_name(&p, "attribute")?;
            if a == name {
                return Err(format!("`{name}` cannot be both the lookup and the result"));
            }
            kit::readable_any(ctx, &g, a)?
        } else {
            (self.dummy.clone(), AttrType::F32)
        };
        let ty = target_type(&g, name, kit::type_named(p.str("type")));
        let hdr = kit::header_of(&g)?;
        let a = kit::writable(ctx, &mut g, name, ty, Missing::Zero)?;
        let mut w = Packer::<80>::new();
        for x in desc(ty).into_iter().chain(desc(u_ty)) {
            w.u32(x);
        }
        w.u32(channels as u32)
            .u32(samples as u32)
            .u32(p.i64("first").clamp(0, i64::from(u32::MAX >> 1)) as u32)
            .u32(lookup)
            .u32(u32::from(p.bool("wrap")))
            .u32(menu_index(COMBINE, p.str("operation")))
            .u32(0)
            .u32(0)
            .vec4([p.f32("scale"), p.f32("amount"), 0.0, 0.0]);
        self.kernel.run(ctx, &hdr, &[&a, &u, &buf], w.bytes());
        *ctx.output = g;
        Ok(())
    }
}

// ------------------------------------------------------------- Falloff Field

const SHAPES: &[&str] = &["sphere", "box", "plane", "cylinder"];
const CURVES: &[&str] = &["smooth", "linear", "step"];

pub fn falloff_schema() -> OperatorSchema {
    let mut params = vec![
        ParamSchema::string("name", "Name", "falloff").help("The float attribute written (a weight, 1 inside the shape to 0 outside)."),
        ParamSchema::menu("shape", "Shape", SHAPES, "sphere").help("sphere: around Centre within Radius; box: inside Size around Centre, fading over Radius outside it; plane: on the Direction side of the plane through Centre, fading over Radius; cylinder: around the axis through Centre along Direction, within Radius."),
        ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("The shape's centre (a point on the plane or the axis)."),
        ParamSchema::float("radius", "Radius", 1.0, Some(0.0), None).help("sphere, cylinder: the radius; box, plane: the width of the fade."),
        ParamSchema::vec("size", "Size", &[1.0, 1.0, 1.0]).help("box: the full weight region's extent along X, Y and Z."),
        ParamSchema::vec("direction", "Direction", &[0.0, 1.0, 0.0]).help("plane: its normal (the weight is 1 on this side); cylinder: its axis."),
        ParamSchema::float("inner", "Inner", 0.0, Some(0.0), Some(1.0)).help("sphere, cylinder: the fraction of Radius at full weight before the fade starts."),
        ParamSchema::menu("curve", "Curve", CURVES, "smooth").help("The fade: smooth (smoothstep), linear, or step (no fade: 1 inside, 0 outside)."),
        ParamSchema::toggle("invert", "Invert", false).help("1 − the weight: full outside the shape."),
    ];
    params.extend(combine_params("set"));
    crate::pnt(
        "falloff_field",
        "Falloff Field",
        "A falloff weight per point from a shape, as Notch-style effectors use: a sphere, a box, a half-space or a cylinder, with a smooth, linear or hard edge, written into a float attribute (`falloff`) and combined with what is there (set, add, multiply, max, min), so several fields layer. Read it with Attribute Math (mix by the weight), Colour by Attribute, Delete or a Kernel.",
        &[("in", false)],
        params,
        40,
    )
}

const FALLOFF: &str = "struct Params {
    centre: vec4f,
    size: vec4f,
    dir: vec4f,
    // radius, inner, amount, unused
    r: vec4f,
    d: vec4u,
    shape: u32,
    curve: u32,
    invert: u32,
    op: u32,
};
@group(0) @binding(2) var<storage, read_write> A: array<u32>;
@group(0) @binding(3) var<storage, read> P: array<vec4f>;
";

const FALLOFF_BODY: &str = "fn fade(t: f32) -> f32 {
    // t: 0 at full weight, 1 at the outer edge.
    let x = clamp(1.0 - t, 0.0, 1.0);
    switch params.curve {
        case 1u: { return x; }
        case 2u: { return select(0.0, 1.0, t < 1.0); }
        default: { return x * x * (3.0 - 2.0 * x); }
    }
}
fn radial(d: f32) -> f32 {
    let r = params.r.x;
    let inner = r * params.r.y;
    if (d <= inner) { return 1.0; }
    if (r <= inner) { return 0.0; }
    return fade((d - inner) / (r - inner));
}
fn point(i: u32) {
    let q = P[i].xyz - params.centre.xyz;
    var w = 0.0;
    switch params.shape {
        case 1u: {
            let out = max(abs(q) - params.size.xyz * 0.5, vec3f(0.0));
            let d = length(out);
            if (params.r.x <= 0.0) { w = select(0.0, 1.0, d <= 0.0); }
            else { w = fade(d / params.r.x); }
        }
        case 2u: {
            let h = dot(q, params.dir.xyz);
            if (params.r.x <= 0.0) { w = select(0.0, 1.0, h >= 0.0); }
            else { w = fade(clamp(-h / params.r.x, 0.0, 2.0)); }
        }
        case 3u: {
            let along = dot(q, params.dir.xyz);
            w = radial(length(q - params.dir.xyz * along));
        }
        default: { w = radial(length(q)); }
    }
    if (params.invert == 1u) { w = 1.0 - w; }
    st_A(i, combine(ld_A(i), vec4f(w), params.op, params.r.z));
}
";

pub struct FalloffField {
    kernel: PointKernel,
}

pub fn create_falloff(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(FalloffField {
        kernel: PointKernel::new(
            device,
            "pnt.falloff_field",
            &format!(
                "{FALLOFF}{}{COMBINE_WGSL}{FALLOFF_BODY}",
                typed_access("A", "params.d", false)
            ),
            &[Bind::ReadWrite, Bind::Read],
            96,
        ),
    })
}

impl GeoOp for FalloffField {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let name = attr_name(&p, "name")?;
        if name == names::P {
            return Err("Falloff Field reads `P`; write another attribute".into());
        }
        let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let ty = target_type(&g, name, AttrType::F32);
        let hdr = kit::header_of(&g)?;
        let a = kit::writable(ctx, &mut g, name, ty, Missing::Zero)?;
        let mut u = Packer::<96>::new();
        u.vec4(vec3(&p, "centre"))
            .vec4(vec3(&p, "size"))
            .vec4(unit(&p, "direction"))
            .vec4([
                p.f32("radius").max(0.0),
                p.f32("inner").clamp(0.0, 1.0),
                p.f32("amount"),
                0.0,
            ]);
        for w in desc(ty) {
            u.u32(w);
        }
        u.u32(menu_index(SHAPES, p.str("shape")))
            .u32(menu_index(CURVES, p.str("curve")))
            .u32(u32::from(p.bool("invert")))
            .u32(menu_index(COMBINE, p.str("operation")));
        self.kernel.run(ctx, &hdr, &[&a, &pos], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}
