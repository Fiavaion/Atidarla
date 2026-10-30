//! `pnt.transform`: translate, rotate and scale about a pivot. Moves `P`;
//! turns the velocity `v` with the same rotation and scale, and the normal
//! `N` with the inverse-transpose (renormalised), when they exist.
//!
//! The matrix is composed on the CPU (scale, then rotation X, Y, Z, then
//! translation), so a chain of transforms costs one 3 × 4 matrix each. One
//! kernel moves `P`, `v` and `N`; its `point` is also the fused stage.

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp, Params, PointStage, STAGE_BYTES};
use atd_gpu::geo::kernels::{Bind, Packer};

use crate::kit::{self, Missing, PointKernel, vec3};

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "transform",
        "Transform",
        "Translates, rotates and scales points about a pivot: moves `P`, and turns `v` (rotation and scale) and `N` (kept perpendicular to surfaces, unit length) when they exist.",
        &[("in", false)],
        vec![
            ParamSchema::vec("translate", "Translate", &[0.0, 0.0, 0.0]).help("Moves the points."),
            ParamSchema::vec("rotate", "Rotate", &[0.0, 0.0, 0.0])
                .help("Rotation about X, then Y, then Z, in degrees, about the pivot."),
            ParamSchema::vec("scale", "Scale", &[1.0, 1.0, 1.0])
                .help("Scale along X, Y and Z about the pivot."),
            ParamSchema::float("uniform_scale", "Uniform Scale", 1.0, None, None)
                .help("Multiplies the scale on every axis."),
            ParamSchema::vec("pivot", "Pivot", &[0.0, 0.0, 0.0])
                .help("The point rotation and scale are about."),
        ],
        60,
    )
}

/// A 3 × 3 matrix, rows.
type M3 = [[f32; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    m
}

/// Rotation X then Y then Z (degrees), as `atd_euler` in WGSL.
fn rotation(deg: [f32; 4]) -> M3 {
    let [x, y, z] = [deg[0], deg[1], deg[2]].map(f32::to_radians);
    let rx = [
        [1.0, 0.0, 0.0],
        [0.0, x.cos(), -x.sin()],
        [0.0, x.sin(), x.cos()],
    ];
    let ry = [
        [y.cos(), 0.0, y.sin()],
        [0.0, 1.0, 0.0],
        [-y.sin(), 0.0, y.cos()],
    ];
    let rz = [
        [z.cos(), -z.sin(), 0.0],
        [z.sin(), z.cos(), 0.0],
        [0.0, 0.0, 1.0],
    ];
    mul(&rz, &mul(&ry, &rx))
}

fn diag(s: [f32; 3]) -> M3 {
    [[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]]
}

/// The point matrix (rows, `w`: translation), the vector matrix (rows) and
/// the normal matrix (rows) of the parameters.
pub(crate) fn matrices(p: &Params<'_>) -> ([[f32; 4]; 3], M3, M3) {
    let u = p.f32("uniform_scale");
    let s = vec3(p, "scale");
    let s = [s[0] * u, s[1] * u, s[2] * u];
    let r = rotation(vec3(p, "rotate"));
    let m = mul(&r, &diag(s));
    let inv = s.map(|c| if c.abs() < 1e-20 { 0.0 } else { 1.0 / c });
    let n = mul(&r, &diag(inv));
    // P' = M (P − pivot) + pivot + translate.
    let pivot = vec3(p, "pivot");
    let t = vec3(p, "translate");
    let mut rows = [[0.0; 4]; 3];
    for k in 0..3 {
        let mp: f32 = (0..3).map(|c| m[k][c] * pivot[c]).sum();
        rows[k] = [m[k][0], m[k][1], m[k][2], pivot[k] + t[k] - mp];
    }
    (rows, m, n)
}

/// The uniforms: the point matrix's rows (translation in `w`), the normal
/// matrix's rows, and which of `v` (bit 0) and `N` (bit 1) to turn.
pub(crate) const DECL: &str = "struct Params {
    p0: vec4f,
    p1: vec4f,
    p2: vec4f,
    n0: vec4f,
    n1: vec4f,
    n2: vec4f,
    // x: bit 0 turns `v`, bit 1 `N`; y: always 0 (`crate::fuse::OPAQUE`)
    has: vec4u,
};
";

/// Byte offset of `has` in the uniforms (the fuser sets it per stage).
pub(crate) const HAS_OFFSET: usize = 96;

/// The stage over `P`, `v` and `N` (as `crate::fuse::stage_function` turns
/// it into a function of local values, shared with the fused kernel).
pub(crate) const BODY: &str = "fn point(i: u32) {
    let x = vec4f(P[i].xyz, 1.0);
    P[i] = vec4f(dot(params.p0, x), dot(params.p1, x), dot(params.p2, x), P[i].w);
    if ((params.has.x & 1u) != 0u) {
        let w = vec4f(v[i].xyz, 0.0);
        let y = vec3f(
            dot(vec4f(params.p0.xyz, 0.0), w),
            dot(vec4f(params.p1.xyz, 0.0), w),
            dot(vec4f(params.p2.xyz, 0.0), w),
        );
        v[i] = vec4f(y, v[i].w);
    }
    if ((params.has.x & 2u) != 0u) {
        let w = vec4f(N[i].xyz, 0.0);
        var y = vec3f(dot(params.n0, w), dot(params.n1, w), dot(params.n2, w));
        let l = length(y);
        if (l > 1e-20) { y = y / l; }
        N[i] = vec4f(y, N[i].w);
    }
}
";

const BINDINGS: &str = "@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> v: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> N: array<vec4f>;
";

/// The unfused kernel after the prelude: loads the point, runs the stage,
/// stores what it turned.
fn kernel_source() -> String {
    format!(
        "{DECL}{BINDINGS}{}{}
fn point(i: u32) {{
    let z = params.has.y;
    var p = atd_opaque(P[i], z);
    var w = vec4f(0.0);
    var n = vec4f(0.0);
    if ((params.has.x & 1u) != 0u) {{ w = atd_opaque(v[i], z); }}
    if ((params.has.x & 2u) != 0u) {{ n = atd_opaque(N[i], z); }}
    {}(&p, &w, &n, params);
    P[i] = p;
    if ((params.has.x & 1u) != 0u) {{ v[i] = w; }}
    if ((params.has.x & 2u) != 0u) {{ N[i] = n; }}
}}
",
        crate::fuse::OPAQUE,
        crate::fuse::stage_function("transform", BODY, "Params"),
        crate::fuse::function("transform"),
    )
}

/// The uniforms of `params`, with `has` left at 0.
pub(crate) fn uniforms(params: &Params<'_>) -> Packer<112> {
    let (rows, _, n) = matrices(params);
    let mut u = Packer::<112>::new();
    for row in rows {
        u.vec4(row);
    }
    for row in n {
        u.vec4([row[0], row[1], row[2], 0.0]);
    }
    u
}

pub struct Transform {
    kernel: PointKernel,
    /// Bound for a missing `v` or `N` (never written).
    unused: [wgpu::Buffer; 2],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    let unused = || {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.transform.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        })
    };
    Box::new(Transform {
        kernel: PointKernel::new(
            device,
            "pnt.transform",
            &kernel_source(),
            &[ReadWrite, ReadWrite, ReadWrite],
            112,
        ),
        unused: [unused(), unused()],
    })
}

impl GeoOp for Transform {
    fn point_stage(&self, params: &Params<'_>, _dt: f64) -> Option<PointStage> {
        let mut out = [0u8; STAGE_BYTES];
        out[..112].copy_from_slice(uniforms(params).bytes());
        Some(PointStage {
            kind: "transform",
            uniforms: out,
            velocity: false,
        })
    }

    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let hdr = kit::header_of(&g)?;
        let pos = kit::writable(ctx, &mut g, names::P, AttrType::VEC3, Missing::Error)?;
        let mut has = 0u32;
        let vel = if g.point_typed(names::V, AttrType::VEC3).is_some() {
            has |= 1;
            kit::writable(ctx, &mut g, names::V, AttrType::VEC3, Missing::Error)?
        } else {
            self.unused[0].clone()
        };
        let nrm = if g.point_typed(names::N, AttrType::VEC3).is_some() {
            has |= 2;
            kit::writable(ctx, &mut g, names::N, AttrType::VEC3, Missing::Error)?
        } else {
            self.unused[1].clone()
        };
        let mut u = uniforms(&ctx.params);
        u.u32(has);
        self.kernel.run(ctx, &hdr, &[&pos, &vel, &nrm], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_is_valid_wgsl() {
        let src = format!(
            "{}{}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@compute @workgroup_size(256) fn main(@builtin(global_invocation_id) gid: vec3u) {{ if (gid.x < hdr.count) {{ point(gid.x); }} }}",
            kit::prelude(),
            kernel_source()
        );
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .unwrap();
    }
}
