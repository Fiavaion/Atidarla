//! Point sources and combinations: Scatter (random points on a shape),
//! Merge (up to four geometries in one) and Copy to Points (a shape copied
//! onto every point of another geometry).
//!
//! Counts the CPU knows stay on the CPU; when an input's count is only on
//! the GPU (a simulation), the result's count is computed on the GPU too
//! (a one-thread header kernel), so nothing waits for a readback.

use std::sync::Arc;

use atd_core::geo::{AttrClass, AttrType, header, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{Bind, Kernel, Packer, Uniforms, Work, pass};
use atd_gpu::geo::{Attribute, Geometry};

use crate::kit::{self, PointKernel, menu_index, vec3};
use crate::select::MAX_ATTRS;

/// Most points a source makes (a 1-D dispatch reaches 65,535 × 256).
pub const MAX_POINTS: u64 = 16_000_000;

fn storage(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: atd_core::geo::align_up(bytes),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// A header with no points, bound for missing inputs.
fn empty_header(device: &wgpu::Device) -> wgpu::Buffer {
    use wgpu::util::DeviceExt as _;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pnt.empty_header"),
        contents: &header::bytes(0, 0),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

/// Checks a point count against the dispatch and binding limits.
fn check_points(device: &wgpu::Device, points: u64) -> Result<u32, String> {
    let max = (device.limits().max_storage_buffer_binding_size / 16).min(MAX_POINTS);
    if points > max {
        return Err(format!(
            "{points} points is more than this GPU can hold here ({max})"
        ));
    }
    Ok(points as u32)
}

// ------------------------------------------------------------------ Scatter

const SHAPES: &[&str] = &[
    "line",
    "circle",
    "disk",
    "rectangle",
    "sphere",
    "ball",
    "box",
];

pub fn scatter_schema() -> OperatorSchema {
    crate::pnt(
        "scatter",
        "Scatter",
        "Random points, uniformly distributed on a shape: a line segment, a circle or a disk, a rectangle, a sphere's surface, a ball or a box. Writes `P`, `id` and, on the flat and spherical shapes, the surface normal `N`. Seeded: the same seed gives the same points. (Scattering on a mesh surface comes with the MSH operators.)",
        &[],
        vec![
            ParamSchema::menu("shape", "Shape", SHAPES, "disk").help(
                "line: Start to End; circle, disk: radius Radius in the XY plane; rectangle: Size X by Y in the XY plane; sphere: the surface of radius Radius; ball: inside it; box: inside Size. Then rotated by Rotate and moved to Centre.",
            ),
            ParamSchema::int("count", "Count", 10_000, Some(0), Some(MAX_POINTS as i64)).help("Number of points."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("Centre of the shape (not used by line)."),
            ParamSchema::float("radius", "Radius", 1.0, Some(0.0), None).help("circle, disk, sphere, ball: the radius."),
            ParamSchema::float("inner", "Inner", 0.0, Some(0.0), Some(1.0)).help("disk, ball: the hole's radius, as a fraction of Radius."),
            ParamSchema::vec("size", "Size", &[2.0, 2.0, 2.0]).help("rectangle, box: the extent along X, Y and Z."),
            ParamSchema::vec("start", "Start", &[-1.0, 0.0, 0.0]).help("line: one end."),
            ParamSchema::vec("end", "End", &[1.0, 0.0, 0.0]).help("line: the other end."),
            ParamSchema::vec("rotate", "Rotate", &[0.0, 0.0, 0.0]).help("Rotation of the shape about X, Y, Z in degrees (not used by line)."),
            ParamSchema::int("seed", "Seed", 1, Some(0), None).help("Random seed."),
        ],
        150,
    )
}

const SCATTER: &str = r#"struct Params {
    centre: vec4f,
    // radius, inner fraction, count, unused
    r: vec4f,
    size: vec4f,
    start: vec4f,
    end: vec4f,
    rotate: vec4f,
    shape: u32,
    seed: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> N: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> ID: array<u32>;

fn point(i: u32) {
    let u = atd_rand3(params.seed, i);
    let R = params.r.x;
    var p = vec3f(0.0);
    var n = vec3f(0.0, 0.0, 1.0);
    switch params.shape {
        case 0u: {
            P[i] = vec4f(mix(params.start.xyz, params.end.xyz, u.x), 0.0);
            N[i] = vec4f(0.0);
            ID[i] = i;
            return;
        }
        case 1u: {
            let a = u.x * 6.28318530718;
            p = vec3f(cos(a), sin(a), 0.0) * R;
        }
        case 2u: {
            let a = u.x * 6.28318530718;
            let k = params.r.y * params.r.y;
            p = vec3f(cos(a), sin(a), 0.0) * (R * sqrt(mix(k, 1.0, u.y)));
        }
        case 3u: {
            p = vec3f((u.x - 0.5) * params.size.x, (u.y - 0.5) * params.size.y, 0.0);
        }
        case 4u, 5u: {
            let z = u.x * 2.0 - 1.0;
            let a = u.y * 6.28318530718;
            let s = sqrt(max(0.0, 1.0 - z * z));
            let d = vec3f(s * cos(a), s * sin(a), z);
            n = d;
            var r = R;
            if (params.shape == 5u) {
                let k = params.r.y * params.r.y * params.r.y;
                r = R * pow(mix(k, 1.0, u.z), 1.0 / 3.0);
                n = vec3f(0.0);
            }
            p = d * r;
        }
        default: {
            p = (u - vec3f(0.5)) * params.size.xyz;
            n = vec3f(0.0);
        }
    }
    P[i] = vec4f(atd_euler(p, params.rotate.xyz) + params.centre.xyz, 0.0);
    N[i] = vec4f(atd_euler(n, params.rotate.xyz), 0.0);
    ID[i] = i;
}
"#;

pub struct Scatter {
    kernel: PointKernel,
    names: [Arc<str>; 3],
}

pub fn create_scatter(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Scatter {
        kernel: PointKernel::new(
            device,
            "pnt.scatter",
            SCATTER,
            &[ReadWrite, ReadWrite, ReadWrite],
            112,
        ),
        names: [names::P, names::N, names::ID].map(Arc::from),
    })
}

impl GeoOp for Scatter {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let count = match check_points(ctx.device, p.i64("count").max(0) as u64) {
            Ok(n) => n,
            Err(e) => {
                ctx.output.clear();
                return Err(e);
            }
        };
        let shape = menu_index(SHAPES, p.str("shape"));
        let mut u = Packer::<112>::new();
        u.vec4(vec3(&p, "centre"))
            .vec4([
                p.f32("radius").max(0.0),
                p.f32("inner").clamp(0.0, 1.0),
                0.0,
                0.0,
            ])
            .vec4(vec3(&p, "size"))
            .vec4(vec3(&p, "start"))
            .vec4(vec3(&p, "end"))
            .vec4(vec3(&p, "rotate"))
            .u32(shape)
            .u32(kit::seed(&p));
        let header = ctx.cpu_header(count);
        let pos = ctx.alloc(self.names[0].clone(), AttrType::VEC3, count);
        let nrm = ctx.alloc(self.names[1].clone(), AttrType::VEC3, count);
        let id = ctx.alloc(self.names[2].clone(), AttrType::U32, count);
        if count > 0 {
            let b = [&pos, &nrm, &id].map(|a| a.gpu_buffer().expect("on the GPU").clone());
            self.kernel
                .run(ctx, header.buffer(), &[&b[0], &b[1], &b[2]], u.bytes());
        }
        let out = &mut *ctx.output;
        out.clear();
        out.set_points(count, Some(header));
        out.set_point(pos);
        if shape != 0 {
            out.set_point(nrm);
        }
        out.set_point(id);
        Ok(())
    }
}

// -------------------------------------------------------------------- Merge

/// Inputs Merge combines.
const MERGE_INPUTS: usize = 4;

pub fn merge_schema() -> OperatorSchema {
    crate::pnt(
        "merge",
        "Merge",
        "Combines up to four geometries into one: their points one after the other (a, then b, c, d). Attributes are the union of the inputs'; points from an input without one get zero (an attribute whose type differs from the first input's is treated as missing). Groups are not carried; primitives are dropped.",
        &[("a", false), ("b", false), ("c", false), ("d", false)],
        vec![],
        150,
    )
}

/// Header kernel: one slot per input (`offset`, `count`) and the result's
/// header, from the inputs' headers.
const MERGE_HEADER: &str = "struct Params { caps: vec4u };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> h0: GeoHeader;
@group(0) @binding(2) var<storage, read> h1: GeoHeader;
@group(0) @binding(3) var<storage, read> h2: GeoHeader;
@group(0) @binding(4) var<storage, read> h3: GeoHeader;
@group(0) @binding(5) var<storage, read_write> slots: array<vec4u, 4>;
@group(0) @binding(6) var<storage, read_write> hdr: GeoHeader;
@compute @workgroup_size(1)
fn main() {
    let c = vec4u(h0.count, h1.count, h2.count, h3.count);
    slots[0] = vec4u(0u, c.x, 0u, 0u);
    slots[1] = vec4u(c.x, c.y, 0u, 0u);
    slots[2] = vec4u(c.x + c.y, c.z, 0u, 0u);
    slots[3] = vec4u(c.x + c.y + c.z, c.w, 0u, 0u);
    let count = c.x + c.y + c.z + c.w;
    hdr.count = count;
    hdr.capacity = params.caps.x + params.caps.y + params.caps.z + params.caps.w;
    hdr.mapped = count;
    hdr.pad = 0u;
    hdr.draw[0] = 4u;
    hdr.draw[1] = count;
    hdr.draw[2] = 0u;
    hdr.draw[3] = 0u;
    hdr.dispatch[0] = max(1u, (count + 255u) / 256u);
    hdr.dispatch[1] = 1u;
    hdr.dispatch[2] = 1u;
    hdr.dispatch[3] = 0u;
}
";

/// Place kernel for one element size: `dst[offset + i] = src[i]`.
fn place_source(ty: &str) -> String {
    format!(
        "{}
@group(0) @binding(0) var<storage, read> slot: vec4u;
@group(0) @binding(1) var<storage, read> src: array<{ty}>;
@group(0) @binding(2) var<storage, read_write> dst: array<{ty}>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    if (i < slot.y) {{ dst[slot.x + i] = src[i]; }}
}}
",
        header::WGSL
    )
}

pub struct Merge {
    header: Kernel,
    caps: Uniforms,
    /// One per element size (4, 8, 16 bytes).
    place: [Kernel; 3],
    /// One (offset, count) slot per input; each is bound on its own.
    slots: wgpu::Buffer,
    slot_views: [wgpu::Buffer; MERGE_INPUTS],
    empty: wgpu::Buffer,
    /// Bound by the header kernel; copied into the pooled output header.
    work: wgpu::Buffer,
}

pub fn create_merge(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    let place = |ty: &str, label: &str| {
        Kernel::new(
            device,
            label,
            &place_source(ty),
            "main",
            &[Read, Read, ReadWrite],
        )
    };
    let slots = storage(device, "pnt.merge.slots", 64);
    Box::new(Merge {
        header: Kernel::new(
            device,
            "pnt.merge.header",
            &format!("{}{MERGE_HEADER}", header::WGSL),
            "main",
            &[Uniform, Read, Read, Read, Read, ReadWrite, ReadWrite],
        ),
        caps: Uniforms::new(device, "pnt.merge.caps", 16),
        place: [
            place("u32", "pnt.merge.place4"),
            place("vec2u", "pnt.merge.place8"),
            place("vec4u", "pnt.merge.place16"),
        ],
        slot_views: std::array::from_fn(|_| storage(device, "pnt.merge.slot", 16)),
        slots,
        empty: empty_header(device),
        work: kit::work_header(device),
    })
}

fn place_index(stride: u32) -> usize {
    match stride {
        4 => 0,
        8 => 1,
        _ => 2,
    }
}

impl GeoOp for Merge {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        // The connected inputs with points, in socket order.
        let mut inputs: [Option<Geometry>; MERGE_INPUTS] = Default::default();
        for (k, slot) in inputs.iter_mut().enumerate() {
            *slot = ctx.input(k).filter(|g| g.points() > 0).cloned();
        }
        let caps: [u32; MERGE_INPUTS] =
            std::array::from_fn(|k| inputs[k].as_ref().map_or(0, Geometry::points));
        let total = check_points(ctx.device, caps.iter().map(|&c| u64::from(c)).sum())?;
        let out = &mut *ctx.output;
        out.clear();
        if total == 0 {
            return Ok(());
        }
        // The union of attributes: the first input's types win.
        let mut union: [Option<(Arc<str>, AttrType)>; MAX_ATTRS] = Default::default();
        let mut n = 0;
        for g in inputs.iter().flatten() {
            for a in g.attrs(AttrClass::Point) {
                if union[..n]
                    .iter()
                    .flatten()
                    .any(|(name, _)| &**name == a.name())
                {
                    continue;
                }
                if n == MAX_ATTRS {
                    return Err(format!("more than {MAX_ATTRS} point attributes"));
                }
                union[n] = Some((a.name_arc().clone(), a.ty()));
                n += 1;
            }
        }
        let headers: [wgpu::Buffer; MERGE_INPUTS] = std::array::from_fn(|k| {
            inputs[k]
                .as_ref()
                .and_then(|g| g.header().cloned())
                .unwrap_or_else(|| self.empty.clone())
        });
        let gpu_counted = inputs.iter().flatten().any(Geometry::is_gpu_counted);
        let header = kit::new_header(ctx);
        let hdr = self.work.clone();
        self.caps
            .write(ctx.queue, &atd_gpu::geo::kernels::words(caps));
        // The result's attributes (zeroed where an input lacks one).
        let mut dsts: [Option<Attribute>; MAX_ATTRS] = Default::default();
        for (k, (name, ty)) in union[..n].iter().flatten().enumerate() {
            let a = ctx.alloc(name.clone(), *ty, total);
            let complete = inputs
                .iter()
                .flatten()
                .all(|g| g.point_typed(name, *ty).is_some());
            if !complete && !ctx.params.mode().is_offline() {
                ctx.encoder
                    .clear_buffer(a.gpu_buffer().expect("on the GPU"), 0, None);
            }
            dsts[k] = Some(a);
        }
        // Slot k is copied out of the slots buffer into its own small
        // buffer, so each place dispatch binds a fixed buffer.
        let device = ctx.device;
        {
            let mut p = pass(ctx.encoder);
            self.header.dispatch_in(
                device,
                &mut p,
                &[
                    self.caps.buffer(),
                    &headers[0],
                    &headers[1],
                    &headers[2],
                    &headers[3],
                    &self.slots,
                    &hdr,
                ],
                Work::Groups(1),
            );
        }
        for (k, view) in self.slot_views.iter().enumerate() {
            ctx.encoder
                .copy_buffer_to_buffer(&self.slots, k as u64 * 16, view, 0, 16);
        }
        ctx.encoder
            .copy_buffer_to_buffer(&hdr, 0, header.buffer(), 0, header::BYTES);
        let mut p = pass(ctx.encoder);
        for (k, g) in inputs.iter().enumerate() {
            let Some(g) = g else { continue };
            for (name_ty, dst) in union[..n].iter().zip(&dsts) {
                let (Some((name, ty)), Some(dst)) = (name_ty, dst) else {
                    continue;
                };
                let Some(src) = g.point_typed(name, *ty) else {
                    continue;
                };
                let src = src.ensure_gpu(ctx.queue, ctx.buffers);
                let dst = dst.gpu_buffer().expect("on the GPU");
                self.place[place_index(ty.stride())].dispatch_in(
                    device,
                    &mut p,
                    &[&self.slot_views[k], src, dst],
                    Work::Elements(caps[k]),
                );
            }
        }
        drop(p);
        let out = &mut *ctx.output;
        if gpu_counted {
            out.set_gpu_counted(total, header);
        } else {
            out.set_points(total, Some(header));
        }
        for a in dsts.into_iter().flatten() {
            out.set_point(a);
        }
        Ok(())
    }
}

// --------------------------------------------------------- Copy to Points

pub fn copy_schema() -> OperatorSchema {
    crate::pnt(
        "copy",
        "Copy to Points",
        "Copies the `source` geometry onto every point of `points`: copy t of the source is scaled by Scale and the point's `pscale`, turned by its `orient` (a quaternion) or so the source's +Z follows its `N`, and moved to its `P`. Every source attribute is copied; `copynum` gives the copy's number, and `Cd` can take the point's colour.",
        &[("source", false), ("points", false)],
        vec![
            ParamSchema::float("scale", "Scale", 1.0, None, None).help("Scales every copy."),
            ParamSchema::toggle("use_pscale", "Use pscale", true).help("Scales each copy by the point's `pscale` when it has one."),
            ParamSchema::menu("orient", "Orient", &["none", "orient", "normal"], "orient").help(
                "none: copies keep the source's orientation; orient: by the point's `orient` quaternion; normal: the source's +Z turns to the point's `N` (when the attribute exists).",
            ),
            ParamSchema::toggle("point_colour", "Point Colour", true).help("Multiplies each copy's `Cd` by the point's `Cd` (when the points have one)."),
        ],
        200,
    )
}

const COPY_HEADER: &str = "struct Params { caps: vec4u };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hs: GeoHeader;
@group(0) @binding(2) var<storage, read> ht: GeoHeader;
@group(0) @binding(3) var<storage, read_write> hdr: GeoHeader;
@compute @workgroup_size(1)
fn main() {
    let count = hs.count * ht.count;
    hdr.count = count;
    hdr.capacity = params.caps.x;
    hdr.mapped = count;
    hdr.pad = 0u;
    hdr.draw[0] = 4u;
    hdr.draw[1] = count;
    hdr.draw[2] = 0u;
    hdr.draw[3] = 0u;
    hdr.dispatch[0] = max(1u, (count + 255u) / 256u);
    hdr.dispatch[1] = 1u;
    hdr.dispatch[2] = 1u;
    hdr.dispatch[3] = 0u;
}
";

/// `dst[j] = src[j % source count]` for one element size.
fn repeat_source(ty: &str) -> String {
    format!(
        "{}
@group(0) @binding(0) var<storage, read> hdr: GeoHeader;
@group(0) @binding(1) var<storage, read> hs: GeoHeader;
@group(0) @binding(2) var<storage, read> src: array<{ty}>;
@group(0) @binding(3) var<storage, read_write> dst: array<{ty}>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let j = gid.x;
    if (j < hdr.count) {{ dst[j] = src[j % max(hs.count, 1u)]; }}
}}
",
        header::WGSL
    )
}

const COPY_P: &str = "struct Params {
    // scale, use pscale, orient mode (0 none, 1 orient, 2 N), unused
    s: vec4f,
};
@group(0) @binding(2) var<storage, read> hs: GeoHeader;
@group(0) @binding(3) var<storage, read> SP: array<vec4f>;
@group(0) @binding(4) var<storage, read> TP: array<vec4f>;
@group(0) @binding(5) var<storage, read> TS: array<f32>;
@group(0) @binding(6) var<storage, read> TQ: array<vec4f>;
@group(0) @binding(7) var<storage, read> TN: array<vec4f>;
@group(0) @binding(8) var<storage, read_write> P: array<vec4f>;

// The rotation taking +Z to n (unit), as a quaternion.
fn z_to(n: vec3f) -> vec4f {
    let c = n.z;
    if (c < -0.999999) { return vec4f(1.0, 0.0, 0.0, 0.0); }
    return normalize(vec4f(-n.y, n.x, 0.0, 1.0 + c));
}

fn point(j: u32) {
    let ns = max(hs.count, 1u);
    let s = j % ns;
    let t = j / ns;
    var p = SP[s].xyz * params.s.x;
    if (params.s.y > 0.5) { p = p * TS[t]; }
    if (params.s.z > 1.5) {
        let l = length(TN[t].xyz);
        if (l > 1e-12) { p = atd_qrotate(z_to(TN[t].xyz / l), p); }
    } else if (params.s.z > 0.5) {
        p = atd_qrotate(TQ[t], p);
    }
    P[j] = vec4f(p + TP[t].xyz, 0.0);
}
";

const COPY_EXTRA: &str = "struct Params {
    // x: multiply Cd by the point's; y: the source has no Cd (take the point's)
    f: vec4f,
};
@group(0) @binding(2) var<storage, read> hs: GeoHeader;
@group(0) @binding(3) var<storage, read_write> copynum: array<u32>;
@group(0) @binding(4) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(5) var<storage, read> TC: array<vec4f>;
fn point(j: u32) {
    let t = j / max(hs.count, 1u);
    copynum[j] = t;
    if (params.f.y > 0.5) {
        Cd[j] = TC[t];
    } else if (params.f.x > 0.5) {
        Cd[j] = Cd[j] * TC[t];
    }
}
";

pub struct Copy {
    header: Kernel,
    caps: Uniforms,
    repeat: [Kernel; 3],
    pos: PointKernel,
    extra: PointKernel,
    dummy: wgpu::Buffer,
    /// Bound by the kernels; copied into the pooled output header.
    work: wgpu::Buffer,
    names: [Arc<str>; 3],
}

pub fn create_copy(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    let repeat = |ty: &str, label: &str| {
        Kernel::new(
            device,
            label,
            &repeat_source(ty),
            "main",
            &[Read, Read, Read, ReadWrite],
        )
    };
    Box::new(Copy {
        header: Kernel::new(
            device,
            "pnt.copy.header",
            &format!("{}{COPY_HEADER}", header::WGSL),
            "main",
            &[Uniform, Read, Read, ReadWrite],
        ),
        caps: Uniforms::new(device, "pnt.copy.caps", 16),
        repeat: [
            repeat("u32", "pnt.copy.repeat4"),
            repeat("vec2u", "pnt.copy.repeat8"),
            repeat("vec4u", "pnt.copy.repeat16"),
        ],
        pos: PointKernel::new(
            device,
            "pnt.copy.P",
            COPY_P,
            &[Read, Read, Read, Read, Read, Read, ReadWrite],
            16,
        ),
        extra: PointKernel::new(
            device,
            "pnt.copy.extra",
            COPY_EXTRA,
            &[Read, ReadWrite, ReadWrite, Read],
            16,
        ),
        dummy: storage(device, "pnt.copy.unused", 256),
        work: kit::work_header(device),
        names: [names::P, "copynum", names::CD].map(Arc::from),
    })
}

impl GeoOp for Copy {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let (Some(src), Some(tmpl)) = (ctx.input(0).cloned(), ctx.input(1).cloned()) else {
            ctx.output.clear();
            return Ok(());
        };
        if src.points() == 0 || tmpl.points() == 0 {
            ctx.output.clear();
            return Ok(());
        }
        let total = check_points(
            ctx.device,
            u64::from(src.points()) * u64::from(tmpl.points()),
        )?;
        let p = ctx.params;
        let sp = kit::readable(ctx, &src, names::P, AttrType::VEC3)?;
        let tp = kit::readable(ctx, &tmpl, names::P, AttrType::VEC3)?;
        let opt = |name: &str, ty: AttrType| {
            tmpl.point_typed(name, ty)
                .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone())
        };
        let ts = opt(names::PSCALE, AttrType::F32).filter(|_| p.bool("use_pscale"));
        let tq = opt(names::ORIENT, AttrType::VEC4);
        let tn = opt(names::N, AttrType::VEC3);
        let tc = opt(names::CD, AttrType::VEC4).filter(|_| p.bool("point_colour"));
        let orient = match p.str("orient") {
            "orient" if tq.is_some() => 1.0,
            "normal" if tn.is_some() => 2.0,
            _ => 0.0,
        };
        let src_hdr = kit::header_of(&src)?;
        let t_hdr = kit::header_of(&tmpl)?;
        let n = src.attrs(AttrClass::Point).len();
        if n > MAX_ATTRS {
            return Err(format!("more than {MAX_ATTRS} point attributes"));
        }
        let header = kit::new_header(ctx);
        let hdr = self.work.clone();
        let out_hdr = header.buffer().clone();
        let mut out = Geometry::new();
        if src.is_gpu_counted() || tmpl.is_gpu_counted() {
            out.set_gpu_counted(total, header);
        } else {
            out.set_points(total, Some(header));
        }
        // Every source attribute but P, repeated per copy.
        let mut moves: [Option<(wgpu::Buffer, wgpu::Buffer, u32)>; MAX_ATTRS] = Default::default();
        for (a, slot) in src.attrs(AttrClass::Point).iter().zip(moves.iter_mut()) {
            if a.name() == names::P {
                continue;
            }
            let dst = ctx.alloc(a.name_arc().clone(), a.ty(), total);
            *slot = Some((
                a.ensure_gpu(ctx.queue, ctx.buffers).clone(),
                dst.gpu_buffer().expect("on the GPU").clone(),
                a.ty().stride(),
            ));
            out.set_point(dst);
        }
        let pos = ctx.alloc(self.names[0].clone(), AttrType::VEC3, total);
        let pos_buf = pos.gpu_buffer().expect("on the GPU").clone();
        out.set_point(pos);
        let copynum = ctx.alloc(self.names[1].clone(), AttrType::U32, total);
        let copynum_buf = copynum.gpu_buffer().expect("on the GPU").clone();
        out.set_point(copynum);
        let src_cd = out.point_typed(names::CD, AttrType::VEC4).is_some();
        let cd_buf = if src_cd {
            out.point(names::CD)
                .and_then(Attribute::gpu_buffer)
                .expect("allocated above")
                .clone()
        } else if tc.is_some() {
            // The source has no colour: copies take the points' colour.
            let cd = ctx.alloc(self.names[2].clone(), AttrType::VEC4, total);
            let b = cd.gpu_buffer().expect("on the GPU").clone();
            out.set_point(cd);
            b
        } else {
            self.dummy.clone()
        };
        self.caps
            .write(ctx.queue, &atd_gpu::geo::kernels::words([total, 0, 0, 0]));
        let mut u = Packer::<16>::new();
        u.vec4([
            p.f32("scale"),
            if ts.is_some() { 1.0 } else { 0.0 },
            orient,
            0.0,
        ]);
        self.pos.write(ctx.queue, u.bytes());
        let mut u = Packer::<16>::new();
        let take = tc.is_some() && !src_cd;
        let multiply = tc.is_some() && src_cd;
        u.vec4([
            f32::from(u8::from(multiply)),
            f32::from(u8::from(take)),
            0.0,
            0.0,
        ]);
        self.extra.write(ctx.queue, u.bytes());
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.header.dispatch_in(
            device,
            &mut ps,
            &[self.caps.buffer(), &src_hdr, &t_hdr, &hdr],
            Work::Groups(1),
        );
        for (src_b, dst_b, stride) in moves.iter().flatten() {
            self.repeat[place_index(*stride)].dispatch_in(
                device,
                &mut ps,
                &[&hdr, &src_hdr, src_b, dst_b],
                Work::Header(&hdr),
            );
        }
        let d = &self.dummy;
        self.pos.run_in(
            device,
            &mut ps,
            &hdr,
            &[
                &src_hdr,
                &sp,
                &tp,
                ts.as_ref().unwrap_or(d),
                tq.as_ref().unwrap_or(d),
                tn.as_ref().unwrap_or(d),
                &pos_buf,
            ],
        );
        self.extra.run_in(
            device,
            &mut ps,
            &hdr,
            &[&src_hdr, &copynum_buf, &cd_buf, tc.as_ref().unwrap_or(d)],
        );
        drop(ps);
        ctx.encoder
            .copy_buffer_to_buffer(&hdr, 0, &out_hdr, 0, header::BYTES);
        *ctx.output = out;
        Ok(())
    }
}
