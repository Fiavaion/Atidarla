//! Attribute operators: Attribute (create or set from a constant, noise, a
//! ramp or the point number), Attribute Math, Randomize and Colour (a ramp
//! from an attribute into `Cd`).
//!
//! They work on attributes of any float or integer type: kernels bind the
//! attribute as raw 32-bit words and convert through a type descriptor in
//! the uniforms (`kit::typed_access`), so one pipeline serves every type.

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp, Params};
use atd_gpu::geo::Geometry;
use atd_gpu::geo::kernels::{Bind, Packer};

use crate::kit::{
    self, Missing, PointKernel, TYPES, desc, menu_index, type_named, typed_access, vec3,
};

fn dummy(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pnt.attr.unused"),
        size: 256,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    })
}

/// The attribute name parameter, checked.
fn attr_name<'a>(p: &Params<'a>, param: &str) -> Result<&'a str, String> {
    let name = p.str(param);
    if atd_core::geo::valid_name(name) {
        Ok(name)
    } else {
        Err(format!("`{name}` is not a valid attribute name"))
    }
}

/// Takes the input, or clears the output; `None` when there is nothing to do.
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

// ---------------------------------------------------------------- Attribute

const SOURCES: &[&str] = &["constant", "ramp", "index", "noise"];

pub fn attribute_schema() -> OperatorSchema {
    crate::pnt(
        "attribute",
        "Attribute",
        "Creates or sets a point attribute of any type: a constant, a ramp between two values along an axis, a ramp over the point numbers, or gradient noise at `P`. An attribute of another type with the same name is replaced.",
        &[("in", false)],
        vec![
            ParamSchema::string("name", "Name", "pscale").help("The attribute written."),
            ParamSchema::menu("type", "Type", TYPES, "float").help("Its type (int is a 32-bit signed integer; values are rounded)."),
            ParamSchema::menu("source", "Source", SOURCES, "constant").help(
                "constant: Value; ramp: Value at Start to Value 2 at End, along the line between them; index: Value for the first point to Value 2 for the last; noise: Value + Amplitude × gradient noise at P (a different noise per component).",
            ),
            ParamSchema::vec("value", "Value", &[1.0, 1.0, 1.0, 1.0]).help("The value (components past the type's are ignored)."),
            ParamSchema::vec("value2", "Value 2", &[0.0, 0.0, 0.0, 0.0]).help("ramp, index: the value at the other end."),
            ParamSchema::vec("start", "Start", &[-1.0, 0.0, 0.0]).help("ramp: where the ramp gives Value."),
            ParamSchema::vec("end", "End", &[1.0, 0.0, 0.0]).help("ramp: where it gives Value 2 (clamped beyond both ends)."),
            ParamSchema::float("amplitude", "Amplitude", 1.0, None, None).help("noise: scale of the noise."),
            ParamSchema::float("frequency", "Frequency", 1.0, Some(0.0), None).help("noise: noise cells per unit."),
            ParamSchema::vec("offset", "Offset", &[0.0, 0.0, 0.0]).help("noise: moves the noise field (animate it to make the values flow)."),
            ParamSchema::int("seed", "Seed", 0, Some(0), None).help("noise: selects a different field."),
        ],
        60,
    )
}

const ATTRIBUTE: &str = "struct Params {
    value: vec4f,
    value2: vec4f,
    start: vec4f,
    // end - start; w: 1 / |end - start|²
    span: vec4f,
    // amplitude, frequency, unused, unused
    noise: vec4f,
    offset: vec4f,
    d: vec4u,
    source: u32,
    seed: u32,
    // 1: the attribute written is P itself
    self_p: u32,
    pad: u32,
};
@group(0) @binding(2) var<storage, read_write> A: array<u32>;
@group(0) @binding(3) var<storage, read> P: array<vec4f>;
";

const ATTRIBUTE_BODY: &str = "fn pos(i: u32) -> vec3f {
    if (params.self_p == 1u) { return ld_A(i).xyz; }
    return P[i].xyz;
}
fn point(i: u32) {
    var v = params.value;
    switch params.source {
        case 1u: {
            let t = clamp(dot(pos(i) - params.start.xyz, params.span.xyz) * params.span.w, 0.0, 1.0);
            v = mix(params.value, params.value2, t);
        }
        case 2u: {
            let t = select(0.0, f32(i) / f32(max(hdr.count, 2u) - 1u), hdr.count > 1u);
            v = mix(params.value, params.value2, t);
        }
        case 3u: {
            let q = pos(i) * params.noise.y + params.offset.xyz;
            let n = vec4f(
                atd_gnoise(q, params.seed).x,
                atd_gnoise(q, params.seed + 1u).x,
                atd_gnoise(q, params.seed + 2u).x,
                atd_gnoise(q, params.seed + 3u).x,
            );
            v = params.value + n * params.noise.x;
        }
        default: {}
    }
    st_A(i, v);
}
";

pub struct Attribute {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
}

pub fn create_attribute(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Attribute {
        kernel: PointKernel::new(
            device,
            "pnt.attribute",
            &format!(
                "{ATTRIBUTE}{}{ATTRIBUTE_BODY}",
                typed_access("A", "params.d", false)
            ),
            &[Bind::ReadWrite, Bind::Read],
            128,
        ),
        dummy: dummy(device),
    })
}

impl GeoOp for Attribute {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let name = attr_name(&p, "name")?;
        let ty = type_named(p.str("type"));
        let source = menu_index(SOURCES, p.str("source"));
        let self_p = name == names::P;
        let needs_p = matches!(source, 1 | 3);
        let pos = if needs_p && !self_p {
            kit::readable(ctx, &g, names::P, AttrType::VEC3)?
        } else {
            self.dummy.clone()
        };
        if self_p && ty != AttrType::VEC3 {
            return Err("`P` is a vec3".into());
        }
        let hdr = kit::header_of(&g)?;
        let a = kit::writable(ctx, &mut g, name, ty, Missing::Create)?;
        let s = vec3(&p, "start");
        let e = vec3(&p, "end");
        let span = [e[0] - s[0], e[1] - s[1], e[2] - s[2]];
        let len2 = span.iter().map(|c| c * c).sum::<f32>();
        let o = vec3(&p, "offset");
        let mut u = Packer::<128>::new();
        u.vec4(p.vec4("value"))
            .vec4(p.vec4("value2"))
            .vec4(s)
            .vec4([
                span[0],
                span[1],
                span[2],
                if len2 > 0.0 { 1.0 / len2 } else { 0.0 },
            ])
            .vec4([p.f32("amplitude"), p.f32("frequency").max(0.0), 0.0, 0.0])
            .vec4(o);
        for w in desc(ty) {
            u.u32(w);
        }
        u.u32(source).u32(kit::seed(&p)).u32(u32::from(self_p));
        self.kernel.run(ctx, &hdr, &[&a, &pos], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}

// ------------------------------------------------------------ Attribute Math

const OPS: &[&str] = &[
    "add",
    "subtract",
    "multiply",
    "divide",
    "min",
    "max",
    "power",
    "mix",
    "fit",
    "clamp",
    "length",
    "normalize",
    "abs",
    "negate",
    "floor",
    "fract",
    "sin",
    "cos",
    "set",
];

pub fn math_schema() -> OperatorSchema {
    crate::pnt(
        "attribute_math",
        "Attribute Math",
        "Computes a point attribute from another, component by component: Result = A (op) B, where B is a second attribute or a constant. Arithmetic, min/max, power, mix, fit (remap a range), clamp, and one-input functions (length, normalize, abs, negate, floor, fract, sin, cos, set).",
        &[("in", false)],
        vec![
            ParamSchema::string("a", "A", "P").help("The first operand, a point attribute."),
            ParamSchema::menu("operation", "Operation", OPS, "multiply").help(
                "add, subtract, multiply, divide, min, max, power: A op B; mix: A to B by Amount; fit: A from [Range Low, Range High] to [B, B2]; clamp: A between B and B2; length (a float), normalize, abs, negate, floor, fract, sin, cos: of A; set: B.",
            ),
            ParamSchema::menu("b_from", "B From", &["constant", "attribute"], "constant").help("Whether B is the constant or an attribute."),
            ParamSchema::vec("constant", "Constant", &[1.0, 1.0, 1.0, 1.0]).help("B when it is a constant."),
            ParamSchema::string("b", "B", "v").help("B when it is an attribute."),
            ParamSchema::vec("constant2", "B2", &[1.0, 1.0, 1.0, 1.0]).help("fit: the high end of the output range; clamp: the upper bound."),
            ParamSchema::vec("range", "Range", &[0.0, 1.0]).help("fit: the input range (low, high)."),
            ParamSchema::float("amount", "Amount", 0.5, None, None).help("mix: 0 gives A, 1 gives B."),
            ParamSchema::string("result", "Result", "").help("The attribute written (empty: A itself)."),
            ParamSchema::menu("result_type", "Result Type", &["same", "float", "vec2", "vec3", "vec4", "int"], "same")
                .help("The result's type: the same as A (a float for length), or another."),
        ],
        60,
    )
}

const MATH: &str = "struct Params {
    k: vec4f,
    k2: vec4f,
    // range low, high, mix amount, unused
    m: vec4f,
    dr: vec4u,
    da: vec4u,
    db: vec4u,
    op: u32,
    // bit 0: B is an attribute; 1: A is the result; 2: B is the result
    flags: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(2) var<storage, read_write> R: array<u32>;
@group(0) @binding(3) var<storage, read> A: array<u32>;
@group(0) @binding(4) var<storage, read> B: array<u32>;
";

const MATH_BODY: &str = "fn point(i: u32) {
    var a = ld_A(i);
    if ((params.flags & 2u) != 0u) { a = ld_R(i); }
    var b = params.k;
    if ((params.flags & 1u) != 0u) {
        b = ld_B(i);
        if ((params.flags & 4u) != 0u) { b = ld_R(i); }
    }
    var r = a;
    switch params.op {
        case 0u: { r = a + b; }
        case 1u: { r = a - b; }
        case 2u: { r = a * b; }
        case 3u: { r = select(vec4f(0.0), a / b, b != vec4f(0.0)); }
        case 4u: { r = min(a, b); }
        case 5u: { r = max(a, b); }
        case 6u: { r = sign(a) * pow(abs(a), b); }
        case 7u: { r = mix(a, b, params.m.z); }
        case 8u: {
            let w = params.m.y - params.m.x;
            var t = vec4f(0.0);
            if (w != 0.0) { t = (a - vec4f(params.m.x)) / w; }
            r = mix(b, params.k2, t);
        }
        case 9u: { r = clamp(a, min(b, params.k2), max(b, params.k2)); }
        case 10u: { r = vec4f(length(a), 0.0, 0.0, 0.0); }
        case 11u: {
            let l = length(a);
            r = select(vec4f(0.0), a / l, l > 0.0);
        }
        case 12u: { r = abs(a); }
        case 13u: { r = -a; }
        case 14u: { r = floor(a); }
        case 15u: { r = fract(a); }
        case 16u: { r = sin(a); }
        case 17u: { r = cos(a); }
        default: { r = b; }
    }
    st_R(i, r);
}
";

pub struct Math {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
}

pub fn create_math(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Math {
        kernel: PointKernel::new(
            device,
            "pnt.attribute_math",
            &format!(
                "{MATH}{}{}{}{MATH_BODY}",
                typed_access("R", "params.dr", false),
                typed_access("A", "params.da", true),
                typed_access("B", "params.db", true)
            ),
            &[ReadWrite, Read, Read],
            128,
        ),
        dummy: dummy(device),
    })
}

impl GeoOp for Math {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let a_name = attr_name(&p, "a")?;
        let op = menu_index(OPS, p.str("operation"));
        let result = if p.str("result").is_empty() {
            a_name
        } else {
            attr_name(&p, "result")?
        };
        let (a_buf, a_ty) = kit::readable_any(ctx, &g, a_name)?;
        let b_attr = p.str("b_from") == "attribute";
        let (b_buf, b_ty) = if b_attr {
            kit::readable_any(ctx, &g, attr_name(&p, "b")?)?
        } else {
            (self.dummy.clone(), AttrType::F32)
        };
        let r_ty = match p.str("result_type") {
            "same" if op == menu_index(OPS, "length") => AttrType::F32,
            "same" => a_ty,
            t => type_named(t),
        };
        // An operand that is the result is read through the result's binding
        // (a buffer cannot be bound for reading and writing at once), so it
        // must keep its type.
        let a_is_r = a_name == result;
        let b_is_r = b_attr && p.str("b") == result;
        if (a_is_r && r_ty != a_ty) || (b_is_r && r_ty != b_ty) {
            return Err(format!(
                "writing `{result}` as {r_ty} would replace an operand; choose another Result"
            ));
        }
        let hdr = kit::header_of(&g)?;
        let r = kit::writable(ctx, &mut g, result, r_ty, Missing::Create)?;
        let a_bind = if a_is_r { &self.dummy } else { &a_buf };
        let b_bind = if b_is_r { &self.dummy } else { &b_buf };
        let range = p.vec4("range");
        let mut u = Packer::<128>::new();
        u.vec4(p.vec4("constant")).vec4(p.vec4("constant2")).vec4([
            range[0],
            range[1],
            p.f32("amount"),
            0.0,
        ]);
        for w in desc(r_ty).into_iter().chain(desc(a_ty)).chain(desc(b_ty)) {
            u.u32(w);
        }
        let flags = u32::from(b_attr) | (u32::from(a_is_r) << 1) | (u32::from(b_is_r) << 2);
        u.u32(op).u32(flags);
        self.kernel.run(ctx, &hdr, &[&r, a_bind, b_bind], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}

// ----------------------------------------------------------------- Randomize

const DISTRIBUTIONS: &[&str] = &["uniform", "normal", "direction"];

pub fn randomize_schema() -> OperatorSchema {
    crate::pnt(
        "randomize",
        "Randomize",
        "Sets a point attribute to seeded random values: uniform between Min and Max per component, normal (Gaussian, mean Min, deviation Max), or random unit directions scaled by Max. A function of the seed and each point's `id` (or number), so the same points get the same values on every run and frame.",
        &[("in", false)],
        vec![
            ParamSchema::string("name", "Name", "Cd").help("The attribute written."),
            ParamSchema::menu("type", "Type", TYPES, "vec4").help("Its type."),
            ParamSchema::menu("distribution", "Distribution", DISTRIBUTIONS, "uniform")
                .help("uniform: Min to Max; normal: mean Min, standard deviation Max; direction: unit vectors (the type's components) times Max."),
            ParamSchema::vec("min", "Min", &[0.0, 0.0, 0.0, 1.0]).help("uniform: lowest values; normal: means."),
            ParamSchema::vec("max", "Max", &[1.0, 1.0, 1.0, 1.0]).help("uniform: highest values; normal: deviations; direction: lengths."),
            ParamSchema::int("seed", "Seed", 0, Some(0), None).help("Selects different values."),
            ParamSchema::toggle("by_id", "By ID", true).help("Seeds each point from its `id` attribute when it has one (values follow points that move in the list, such as particles), else from its number."),
        ],
        60,
    )
}

const RANDOM: &str = "struct Params {
    lo: vec4f,
    hi: vec4f,
    d: vec4u,
    dist: u32,
    seed: u32,
    by_id: u32,
    pad: u32,
};
@group(0) @binding(2) var<storage, read_write> A: array<u32>;
@group(0) @binding(3) var<storage, read> ID: array<u32>;
";

const RANDOM_BODY: &str = "fn point(i: u32) {
    var k = i;
    if (params.by_id == 1u) { k = ID[i]; }
    let u0 = vec4f(
        atd_rand(params.seed, k),
        atd_rand(params.seed + 0x9e3779b9u, k),
        atd_rand(params.seed + 0x3c6ef372u, k),
        atd_rand(params.seed + 0xdaa66d2bu, k),
    );
    var v = mix(params.lo, params.hi, u0);
    if (params.dist == 1u) {
        let u1 = vec4f(
            atd_rand(params.seed ^ 0x85ebca6bu, k),
            atd_rand(params.seed ^ 0xc2b2ae35u, k),
            atd_rand(params.seed ^ 0x27d4eb2fu, k),
            atd_rand(params.seed ^ 0x165667b1u, k),
        );
        // Box-Muller: (0, 1] keeps the logarithm finite.
        let r = sqrt(-2.0 * log(1.0 - u0));
        v = params.lo + params.hi * r * cos(6.28318530718 * u1);
    }
    if (params.dist == 2u) {
        var dir = vec4f(0.0);
        if (params.d.x == 1u) {
            dir.x = select(-1.0, 1.0, u0.x < 0.5);
        } else if (params.d.x == 2u) {
            let a = u0.x * 6.28318530718;
            dir = vec4f(cos(a), sin(a), 0.0, 0.0);
        } else {
            let z = u0.x * 2.0 - 1.0;
            let a = u0.y * 6.28318530718;
            let s = sqrt(max(0.0, 1.0 - z * z));
            dir = vec4f(s * cos(a), s * sin(a), z, 0.0);
        }
        v = dir * params.hi;
    }
    st_A(i, v);
}
";

pub struct Randomize {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
}

pub fn create_randomize(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Randomize {
        kernel: PointKernel::new(
            device,
            "pnt.randomize",
            &format!(
                "{RANDOM}{}{RANDOM_BODY}",
                typed_access("A", "params.d", false)
            ),
            &[Bind::ReadWrite, Bind::Read],
            64,
        ),
        dummy: dummy(device),
    })
}

impl GeoOp for Randomize {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let name = attr_name(&p, "name")?;
        let ty = type_named(p.str("type"));
        let id = if p.bool("by_id") && name != names::ID {
            g.point_typed(names::ID, AttrType::U32)
                .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone())
        } else {
            None
        };
        let hdr = kit::header_of(&g)?;
        let a = kit::writable(ctx, &mut g, name, ty, Missing::Create)?;
        let mut u = Packer::<64>::new();
        u.vec4(p.vec4("min")).vec4(p.vec4("max"));
        for w in desc(ty) {
            u.u32(w);
        }
        u.u32(menu_index(DISTRIBUTIONS, p.str("distribution")))
            .u32(kit::seed(&p))
            .u32(u32::from(id.is_some()));
        let id = id.as_ref().unwrap_or(&self.dummy);
        self.kernel.run(ctx, &hdr, &[&a, id], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}

// -------------------------------------------------------------------- Colour

const MEASURES: &[&str] = &["x", "y", "z", "w", "length"];
const RAMPS: &[&str] = &["two", "three", "heat", "ice", "rainbow"];

pub fn colour_schema() -> OperatorSchema {
    crate::pnt(
        "colour",
        "Colour by Attribute",
        "Colours points from an attribute: a component (or the length) of it, mapped from [Low, High] to 0-1, picks a colour from a ramp (two or three colours of your choice, or the heat, ice and rainbow presets) written to `Cd`.",
        &[("in", false)],
        vec![
            ParamSchema::string("attribute", "Attribute", "P").help("The point attribute read (not `Cd`)."),
            ParamSchema::menu("measure", "Measure", MEASURES, "y").help("Which component, or the vector's length."),
            ParamSchema::float("low", "Low", -1.0, None, None).help("The value mapped to the start of the ramp."),
            ParamSchema::float("high", "High", 1.0, None, None).help("The value mapped to its end."),
            ParamSchema::menu("ramp", "Ramp", RAMPS, "two").help("two: Colour 1 to Colour 2; three: Colour 1, Colour 2 in the middle, Colour 3; heat: black, red, yellow, white; ice: black, blue, cyan, white; rainbow: hues."),
            ParamSchema::colour("colour1", "Colour 1", [0.1, 0.2, 1.0, 1.0]).help("The start of the two- and three-colour ramps."),
            ParamSchema::colour("colour2", "Colour 2", [1.0, 0.3, 0.1, 1.0]).help("The end of the two-colour ramp, the middle of the three-colour one."),
            ParamSchema::colour("colour3", "Colour 3", [1.0, 1.0, 0.8, 1.0]).help("The end of the three-colour ramp."),
            ParamSchema::toggle("repeat", "Repeat", false).help("Repeats the ramp past the ends instead of holding the end colours."),
        ],
        60,
    )
}

const COLOUR: &str = "struct Params {
    c1: vec4f,
    c2: vec4f,
    c3: vec4f,
    // low, 1 / (high - low), unused, unused
    range: vec4f,
    d: vec4u,
    measure: u32,
    ramp: u32,
    repeat: u32,
    pad: u32,
};
@group(0) @binding(2) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(3) var<storage, read> X: array<u32>;
";

const COLOUR_BODY: &str = "fn ramp3(a: vec4f, b: vec4f, c: vec4f, t: f32) -> vec4f {
    if (t < 0.5) { return mix(a, b, t * 2.0); }
    return mix(b, c, t * 2.0 - 1.0);
}
fn ramp4(a: vec3f, b: vec3f, c: vec3f, d: vec3f, t: f32) -> vec4f {
    let s = t * 3.0;
    if (s < 1.0) { return vec4f(mix(a, b, s), 1.0); }
    if (s < 2.0) { return vec4f(mix(b, c, s - 1.0), 1.0); }
    return vec4f(mix(c, d, s - 2.0), 1.0);
}
fn point(i: u32) {
    let x = ld_X(i);
    var m = x[min(params.measure, 3u)];
    if (params.measure == 4u) { m = length(x); }
    var t = (m - params.range.x) * params.range.y;
    if (params.repeat == 1u) { t = fract(t); } else { t = clamp(t, 0.0, 1.0); }
    var c = mix(params.c1, params.c2, t);
    switch params.ramp {
        case 1u: { c = ramp3(params.c1, params.c2, params.c3, t); }
        case 2u: { c = ramp4(vec3f(0.0), vec3f(0.9, 0.1, 0.0), vec3f(1.0, 0.85, 0.1), vec3f(1.0), t); }
        case 3u: { c = ramp4(vec3f(0.0), vec3f(0.05, 0.15, 0.8), vec3f(0.2, 0.85, 1.0), vec3f(1.0), t); }
        case 4u: {
            let k = abs(fract(vec3f(t * 0.85) + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0;
            c = vec4f(clamp(k, vec3f(0.0), vec3f(1.0)), 1.0);
        }
        default: {}
    }
    Cd[i] = c;
}
";

pub struct Colour {
    kernel: PointKernel,
}

pub fn create_colour(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Colour {
        kernel: PointKernel::new(
            device,
            "pnt.colour",
            &format!(
                "{COLOUR}{}{COLOUR_BODY}",
                typed_access("X", "params.d", true)
            ),
            &[Bind::ReadWrite, Bind::Read],
            96,
        ),
    })
}

impl GeoOp for Colour {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let name = attr_name(&p, "attribute")?;
        if name == names::CD {
            return Err("colour from another attribute than `Cd` (Colour writes it)".into());
        }
        let (x, ty) = kit::readable_any(ctx, &g, name)?;
        let hdr = kit::header_of(&g)?;
        let cd = kit::writable(ctx, &mut g, names::CD, AttrType::VEC4, Missing::Create)?;
        let (lo, hi) = (p.f32("low"), p.f32("high"));
        let inv = if hi != lo { 1.0 / (hi - lo) } else { 0.0 };
        let mut u = Packer::<96>::new();
        u.vec4(p.vec4("colour1"))
            .vec4(p.vec4("colour2"))
            .vec4(p.vec4("colour3"))
            .vec4([lo, inv, 0.0, 0.0]);
        for w in desc(ty) {
            u.u32(w);
        }
        u.u32(menu_index(MEASURES, p.str("measure")))
            .u32(menu_index(RAMPS, p.str("ramp")))
            .u32(u32::from(p.bool("repeat")));
        self.kernel.run(ctx, &hdr, &[&cd, &x], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}
