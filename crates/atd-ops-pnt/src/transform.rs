//! `pnt.transform`: translate, rotate and scale about a pivot. Moves `P`;
//! turns the velocity `v` with the same rotation and scale, and the normal
//! `N` with the inverse-transpose (renormalised), when they exist.
//!
//! The matrix is composed on the CPU (scale, then rotation X, Y, Z, then
//! translation), so a chain of transforms costs one 3 × 4 matrix each.

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp, Params};
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

const SOURCE: &str = "struct Params {
    r0: vec4f,
    r1: vec4f,
    r2: vec4f,
    // x: 1 to add the translation, y: 1 to renormalise
    flags: vec4f,
};
@group(0) @binding(2) var<storage, read_write> a: array<vec4f>;
fn point(i: u32) {
    let x = vec4f(a[i].xyz, params.flags.x);
    var y = vec3f(dot(params.r0, x), dot(params.r1, x), dot(params.r2, x));
    if (params.flags.y > 0.5) {
        let l = length(y);
        if (l > 1e-20) { y = y / l; }
    }
    a[i] = vec4f(y, a[i].w);
}
";

pub struct Transform {
    /// One kernel per attribute (each has its own uniforms).
    kernels: [PointKernel; 3],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    let k = |label: &str| PointKernel::new(device, label, SOURCE, &[Bind::ReadWrite], 64);
    Box::new(Transform {
        kernels: [
            k("pnt.transform.P"),
            k("pnt.transform.v"),
            k("pnt.transform.N"),
        ],
    })
}

impl GeoOp for Transform {
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
        let (rows, m, n) = matrices(&ctx.params);
        let pack = |r: [[f32; 3]; 3], t: Option<[f32; 3]>, normalise: bool| {
            let mut u = Packer::<64>::new();
            for (k, row) in r.iter().enumerate() {
                u.vec4([row[0], row[1], row[2], t.map_or(0.0, |t| t[k])]);
            }
            u.vec4([
                f32::from(u8::from(t.is_some())),
                f32::from(u8::from(normalise)),
                0.0,
                0.0,
            ]);
            u
        };
        let m3 = [0, 1, 2].map(|k| [rows[k][0], rows[k][1], rows[k][2]]);
        let t = [rows[0][3], rows[1][3], rows[2][3]];
        let pos = kit::writable(ctx, &mut g, names::P, AttrType::VEC3, Missing::Error)?;
        self.kernels[0].run(ctx, &hdr, &[&pos], pack(m3, Some(t), false).bytes());
        if g.point_typed(names::V, AttrType::VEC3).is_some() {
            let v = kit::writable(ctx, &mut g, names::V, AttrType::VEC3, Missing::Error)?;
            self.kernels[1].run(ctx, &hdr, &[&v], pack(m, None, false).bytes());
        }
        if g.point_typed(names::N, AttrType::VEC3).is_some() {
            let nn = kit::writable(ctx, &mut g, names::N, AttrType::VEC3, Missing::Error)?;
            self.kernels[2].run(ctx, &hdr, &[&nn], pack(n, None, true).bytes());
        }
        *ctx.output = g;
        Ok(())
    }
}
