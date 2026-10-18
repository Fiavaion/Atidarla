//! Building blocks shared by this crate's operators: a per-point compute
//! kernel dispatched from the geometry header, attribute access for writing
//! (in place when held uniquely, else a pooled copy) and reading, the
//! attribute-type menu, runtime-typed attribute access in WGSL, and the
//! common WGSL helpers.
//!
//! Every kernel here binds the operator's uniforms at binding 0 and the
//! geometry header at binding 1, then the operator's own buffers, so at most
//! seven attributes per kernel (eight storage buffers per stage is the
//! WebGPU floor, design §7).

use std::sync::Arc;

use atd_core::geo::{AttrClass, AttrType, Scalar, header};
use atd_gpu::engine::GeoCtx;
use atd_gpu::geo::kernels::{Bind, Kernel, Uniforms, Work};
use atd_gpu::geo::{GeoBuffer, Geometry, wgsl};

/// Most buffers an operator binds after the uniforms and the header.
pub const MAX_BINDS: usize = 7;

/// WGSL helpers every kernel of this crate gets (after the shared hash and
/// noise library and the header struct):
///
/// - `atd_rotate(v, axis, angle)`: rotation about a unit axis (Rodrigues);
/// - `atd_qrotate(q, v)`: rotation by a unit quaternion `(x, y, z, w)`;
/// - `atd_euler(v, r)`: rotation by Euler angles in degrees, X then Y then Z;
/// - `atd_falloff(d, radius)`: 1 at the centre, smoothly 0 at the radius
///   (always 1 when the radius is 0 or less);
/// - `atd_fbm_curl(p, seed, octaves)`: curl noise summed over octaves.
pub const HELPERS: &str = r#"
fn atd_rotate(v: vec3f, axis: vec3f, angle: f32) -> vec3f {
    let c = cos(angle);
    let s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

fn atd_qrotate(q: vec4f, v: vec3f) -> vec3f {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

fn atd_euler(v: vec3f, r: vec3f) -> vec3f {
    let a = radians(r);
    var p = v;
    p = vec3f(p.x, p.y * cos(a.x) - p.z * sin(a.x), p.y * sin(a.x) + p.z * cos(a.x));
    p = vec3f(p.x * cos(a.y) + p.z * sin(a.y), p.y, -p.x * sin(a.y) + p.z * cos(a.y));
    p = vec3f(p.x * cos(a.z) - p.y * sin(a.z), p.x * sin(a.z) + p.y * cos(a.z), p.z);
    return p;
}

fn atd_falloff(d: f32, radius: f32) -> f32 {
    if (radius <= 0.0) { return 1.0; }
    let t = clamp(1.0 - d / radius, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn atd_fbm_curl(p: vec3f, seed: u32, octaves: u32) -> vec3f {
    var sum = vec3f(0.0);
    var f = 1.0;
    var a = 1.0;
    for (var o = 0u; o < octaves; o = o + 1u) {
        sum = sum + atd_curl(p * f, seed + o * 3u) * a;
        f = f * 2.0;
        a = a * 0.5;
    }
    return sum;
}
"#;

/// The WGSL every kernel of this crate starts with.
pub fn prelude() -> String {
    format!("{}{}{}{HELPERS}", wgsl::HASH, wgsl::NOISE, header::WGSL)
}

/// A compute kernel over the points of a geometry. `source` declares
/// `struct Params`, the bindings from 2 on (`binds`, in order) and
/// `fn point(i: u32)`, which runs for every live point (`i < hdr.count`);
/// the uniforms (`params`, binding 0) and the header (`hdr`, binding 1)
/// are declared here.
#[derive(Debug)]
pub struct PointKernel {
    kernel: Kernel,
    params: Uniforms,
}

impl PointKernel {
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        source: &str,
        binds: &[Bind],
        params_bytes: u64,
    ) -> Self {
        Self::with_main(
            device,
            label,
            source,
            binds,
            params_bytes,
            "if (i < hdr.count) { point(i); }",
        )
    }

    /// A kernel whose `main` body is `body` (with `i`, the thread index):
    /// dispatch it with [`Self::run_over`].
    pub fn with_main(
        device: &wgpu::Device,
        label: &str,
        source: &str,
        binds: &[Bind],
        params_bytes: u64,
        body: &str,
    ) -> Self {
        assert!(binds.len() <= MAX_BINDS, "{label}: too many bindings");
        let src = format!(
            "{}{source}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@compute @workgroup_size({})
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    {body}
}}
",
            prelude(),
            atd_core::geo::WORKGROUP,
        );
        let mut all = vec![Bind::Uniform, Bind::Read];
        all.extend_from_slice(binds);
        Self {
            kernel: Kernel::new(device, label, &src, "main", &all),
            params: Uniforms::new(device, label, params_bytes.max(16)),
        }
    }

    /// Writes the uniforms (only when they changed); call once per frame.
    pub fn write(&self, queue: &wgpu::Queue, params: &[u8]) {
        self.params.write(queue, params);
    }

    /// Dispatches over the header's count in an open pass.
    pub fn run_in(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::ComputePass<'_>,
        hdr: &wgpu::Buffer,
        buffers: &[&wgpu::Buffer],
    ) {
        self.dispatch_in(device, pass, hdr, buffers, Work::Header(hdr));
    }

    /// Dispatches `work` (for example over a capacity) in an open pass.
    pub fn dispatch_in(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::ComputePass<'_>,
        hdr: &wgpu::Buffer,
        buffers: &[&wgpu::Buffer],
        work: Work<'_>,
    ) {
        let n = buffers.len();
        assert!(n <= MAX_BINDS);
        let mut all: [&wgpu::Buffer; MAX_BINDS + 2] = [hdr; MAX_BINDS + 2];
        all[0] = self.params.buffer();
        all[2..2 + n].copy_from_slice(buffers);
        self.kernel.dispatch_in(device, pass, &all[..2 + n], work);
    }

    /// Writes `params` and dispatches over the header's count in a pass of
    /// its own.
    pub fn run(
        &self,
        ctx: &mut GeoCtx<'_>,
        hdr: &wgpu::Buffer,
        buffers: &[&wgpu::Buffer],
        params: &[u8],
    ) {
        self.write(ctx.queue, params);
        let device = ctx.device;
        let mut pass = atd_gpu::geo::kernels::pass(ctx.encoder);
        self.run_in(device, &mut pass, hdr, buffers);
    }
}

/// What [`writable`] does when the attribute is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// A node error naming it.
    Error,
    /// Create it, cleared to zero.
    Zero,
    /// Create it with undefined contents (the kernel writes every element).
    Create,
}

/// The GPU header of `g`, or a node error.
pub fn header_of(g: &Geometry) -> Result<wgpu::Buffer, String> {
    g.header()
        .cloned()
        .ok_or_else(|| "the input geometry has no GPU header".to_owned())
}

/// Point attribute `name` of `ty` in `g`, ready to be written in place by
/// this frame's kernels: its own buffer when `g` holds it uniquely (the
/// engine handed the geometry over), else a pooled copy (copy on write)
/// that replaces it in `g`. A missing attribute, or one of another type when
/// `missing` is not [`Missing::Error`], is created at `g`'s point count.
pub fn writable(
    ctx: &mut GeoCtx<'_>,
    g: &mut Geometry,
    name: &str,
    ty: AttrType,
    missing: Missing,
) -> Result<wgpu::Buffer, String> {
    match g.point(name) {
        Some(a) if a.ty() == ty => {
            let a = g.attr_mut(AttrClass::Point, name).expect("looked up above");
            if let Some(buffer) = a.unique_gpu() {
                return Ok(buffer.clone());
            }
            let src = a.ensure_gpu(ctx.queue, ctx.buffers).clone();
            let (len, bytes, arc) = (a.len(), a.bytes(), a.name_arc().clone());
            let out = ctx.alloc(arc, ty, len);
            let dst = out.gpu_buffer().expect("allocated on the GPU").clone();
            if bytes > 0 {
                ctx.encoder.copy_buffer_to_buffer(&src, 0, &dst, 0, bytes);
            }
            g.set_point(out);
            Ok(dst)
        }
        Some(a) if missing == Missing::Error => Err(format!(
            "point attribute `{name}` is {}; expected {ty}",
            a.ty()
        )),
        _ if missing == Missing::Error => Err(format!("the input has no point attribute `{name}`")),
        existing => {
            // Reuse the existing name (no allocation) when replacing.
            let arc: Arc<str> = existing.map_or_else(|| Arc::from(name), |a| a.name_arc().clone());
            let out = ctx.alloc(arc, ty, g.points());
            let dst = out.gpu_buffer().expect("allocated on the GPU").clone();
            if missing == Missing::Zero && !ctx.params.mode().is_offline() {
                // Offline, `alloc` has cleared it already.
                ctx.encoder.clear_buffer(&dst, 0, None);
            }
            g.set_point(out);
            Ok(dst)
        }
    }
}

/// Point attribute `name` of `ty` in `g` for reading (uploaded first if it
/// is only on the CPU), or a node error naming it.
pub fn readable(
    ctx: &GeoCtx<'_>,
    g: &Geometry,
    name: &str,
    ty: AttrType,
) -> Result<wgpu::Buffer, String> {
    match g.point(name) {
        Some(a) if a.ty() == ty => Ok(a.ensure_gpu(ctx.queue, ctx.buffers).clone()),
        Some(a) => Err(format!(
            "point attribute `{name}` is {}; expected {ty}",
            a.ty()
        )),
        None => Err(format!("the input has no point attribute `{name}`")),
    }
}

/// Point attribute `name` of any type for reading, with its type.
pub fn readable_any(
    ctx: &GeoCtx<'_>,
    g: &Geometry,
    name: &str,
) -> Result<(wgpu::Buffer, AttrType), String> {
    let a = g
        .point(name)
        .ok_or_else(|| format!("the input has no point attribute `{name}`"))?;
    Ok((a.ensure_gpu(ctx.queue, ctx.buffers).clone(), a.ty()))
}

/// The attribute-type menu of the attribute operators.
pub const TYPES: &[&str] = &["float", "vec2", "vec3", "vec4", "int"];

/// The type a [`TYPES`] entry names.
pub fn type_named(name: &str) -> AttrType {
    match name {
        "vec2" => AttrType::VEC2,
        "vec3" => AttrType::VEC3,
        "vec4" => AttrType::VEC4,
        "int" => AttrType::I32,
        _ => AttrType::F32,
    }
}

/// The runtime descriptor of an attribute type for [`typed_access`]:
/// components, stride in 32-bit words, scalar kind (0 `f32`, 1 `i32`,
/// 2 `u32`), unused.
pub fn desc(ty: AttrType) -> [u32; 4] {
    let kind = match ty.scalar {
        Scalar::F32 => 0,
        Scalar::I32 => 1,
        Scalar::U32 => 2,
    };
    [u32::from(ty.comps()), ty.stride() / 4, kind, 0]
}

/// WGSL access to an attribute bound as raw words (`array<u32>` named
/// `name`) whose type is only known at run time, from the descriptor
/// expression `d` (a `vec4u` from [`desc`]): `ld_<name>(i) -> vec4f`
/// (missing components 0) and, unless `read_only`, `st_<name>(i, v)`
/// (integers rounded).
pub fn typed_access(name: &str, d: &str, read_only: bool) -> String {
    let mut s = format!(
        "fn ld_{name}(i: u32) -> vec4f {{
    var v = vec4f(0.0);
    let b = i * {d}.y;
    for (var c = 0u; c < {d}.x; c = c + 1u) {{
        let w = {name}[b + c];
        var f = bitcast<f32>(w);
        if ({d}.z == 1u) {{ f = f32(bitcast<i32>(w)); }}
        if ({d}.z == 2u) {{ f = f32(w); }}
        v[c] = f;
    }}
    return v;
}}
"
    );
    if !read_only {
        s.push_str(&format!(
            "fn st_{name}(i: u32, v: vec4f) {{
    let b = i * {d}.y;
    for (var c = 0u; c < {d}.x; c = c + 1u) {{
        var w = bitcast<u32>(v[c]);
        if ({d}.z == 1u) {{ w = bitcast<u32>(i32(round(v[c]))); }}
        if ({d}.z == 2u) {{ w = u32(max(round(v[c]), 0.0)); }}
        {name}[b + c] = w;
    }}
}}
"
        ));
    }
    s
}

/// A fresh header from the pool (written by a kernel this frame).
pub fn new_header(ctx: &GeoCtx<'_>) -> Arc<GeoBuffer> {
    Arc::new(ctx.buffers.attr(header::BYTES))
}

/// The index of `value` in a menu's `options` (0 when unknown).
pub fn menu_index(options: &[&str], value: &str) -> u32 {
    options.iter().position(|o| *o == value).unwrap_or(0) as u32
}

/// A `vec` parameter as `[x, y, z, 0]` (the engine's `vec4` gives `w = 1`
/// for three components).
pub fn vec3(p: &atd_gpu::engine::Params<'_>, name: &str) -> [f32; 4] {
    let v = p.vec4(name);
    [v[0], v[1], v[2], 0.0]
}

/// A unit vector from a `vec` parameter (+Y when zero).
pub fn unit(p: &atd_gpu::engine::Params<'_>, name: &str) -> [f32; 4] {
    let v = vec3(p, name);
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-12 {
        [0.0, 1.0, 0.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len, 0.0]
    }
}

/// Seed parameter as `u32`.
pub fn seed(p: &atd_gpu::engine::Params<'_>) -> u32 {
    p.i64("seed").clamp(0, i64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(src: &str) {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn prelude_and_typed_access_are_valid_wgsl() {
        let src = format!(
            "{}
struct Params {{ d: vec4u }};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> a: array<u32>;
{}
@compute @workgroup_size(1) fn main() {{ st_a(0u, ld_a(1u) + vec4f(atd_rotate(vec3f(1.0), vec3f(0.0, 1.0, 0.0), 1.0), atd_falloff(0.5, 1.0))); }}
",
            prelude(),
            typed_access("a", "params.d", false)
        );
        validate(&src);
    }

    #[test]
    fn descriptors_follow_the_layout() {
        assert_eq!(desc(AttrType::VEC3), [3, 4, 0, 0]);
        assert_eq!(desc(AttrType::VEC2), [2, 2, 0, 0]);
        assert_eq!(desc(AttrType::I32), [1, 1, 1, 0]);
        assert_eq!(desc(AttrType::U32), [1, 1, 2, 0]);
        assert_eq!(type_named("vec4"), AttrType::VEC4);
        assert_eq!(menu_index(TYPES, "int"), 4);
    }
}
