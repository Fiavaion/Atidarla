//! Lattice: free-form deformation by a cage of control points (Sederberg
//! and Parry's FFD with trilinear weights). The cage is a box of
//! Divisions control points; each point of the input moves by the
//! trilinear blend of its cell's control-point displacements.
//!
//! The displaced cage comes from the `cage` input (any geometry with one
//! point per control point, in X-fastest order, such as this operator's own
//! cage output sent through deformers) or, unconnected, from smooth noise
//! on the control points. Output `cage` shows the displaced control points
//! with their edges as lines, to see the cage or to deform it further.

use std::sync::Arc;

use lsr_core::geo::{AttrType, names};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params};
use lsr_gpu::geo::kernels::{Bind, Packer, Work, pass};
use lsr_gpu::geo::{Attribute, PrimKind, Topology};

use crate::kit::{self, Missing, PointKernel, menu_index, vec3};

/// Most control points along one axis.
pub const MAX_DIVISIONS: u32 = 16;

const OUTPUTS: &[&str] = &["deformed", "cage"];

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "lattice",
        "Lattice",
        "Free-form deformation: a box of control points (Divisions along X, Y and Z, filling Size around Centre) bends everything inside it as the control points move; each point follows the trilinear blend of the displacements around it (points outside the box follow its nearest face). The displaced control points come from the `cage` input (one point per control point, X fastest, then Y, then Z: for example this operator's cage output sent through Twist or Noise) or, unconnected, from smooth noise of Amplitude. Output cage shows the displaced control points, with the lattice's edges as lines.",
        &[("in", false), ("cage", false)],
        vec![
            ParamSchema::vec("divisions", "Divisions", &[3.0, 3.0, 3.0]).help("Control points along X, Y and Z (2 to 16 each)."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("The centre of the lattice's box."),
            ParamSchema::vec("size", "Size", &[2.0, 2.0, 2.0]).help("The box's extent along X, Y and Z."),
            ParamSchema::float("amplitude", "Amplitude", 0.3, None, None).help("Without a cage input: how far noise moves the control points."),
            ParamSchema::float("frequency", "Frequency", 1.0, Some(0.0), None).help("Without a cage input: the noise's cells per unit."),
            ParamSchema::float("evolution", "Evolution", 0.0, None, None).help("Without a cage input: moves through the noise; drive it from time to make the lattice breathe."),
            ParamSchema::int("seed", "Seed", 0, Some(0), None).help("Without a cage input: selects a different noise."),
            ParamSchema::menu("output", "Output", OUTPUTS, "deformed").help("deformed: the input, deformed; cage: the displaced control points, with the lattice's edges as lines."),
        ],
        80,
    )
}

const COMMON: &str = "struct Params {
    lo: vec4f,
    size: vec4f,
    // amplitude, frequency, unused, unused
    noise: vec4f,
    offset: vec4f,
    // nx, ny, nz, seed
    n: vec4u,
};
fn rest(i: u32) -> vec3f {
    let x = i % params.n.x;
    let y = (i / params.n.x) % params.n.y;
    let z = i / (params.n.x * params.n.y);
    let f = vec3f(f32(x), f32(y), f32(z)) / vec3f(params.n.xyz - vec3u(1u));
    return params.lo.xyz + params.size.xyz * f;
}
";

const CAGE: &str = "@group(0) @binding(2) var<storage, read_write> C: array<vec4f>;
";

const CAGE_MAIN: &str = "let total = params.n.x * params.n.y * params.n.z;
    if (i < total) {
        let r = rest(i);
        // Off the noise's integer lattice, where gradient noise is zero
        // (control points often sit on whole units).
        let q = r * params.noise.y + params.offset.xyz + vec3f(0.5137, 0.2741, 0.8391);
        let d = vec3f(
            lsr_gnoise(q, params.n.w).x,
            lsr_gnoise(q, params.n.w + 1u).x,
            lsr_gnoise(q, params.n.w + 2u).x,
        );
        C[i] = vec4f(r + d * params.noise.x, 0.0);
    }";

const DEFORM: &str = "@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read> C: array<vec4f>;
fn disp(x: u32, y: u32, z: u32) -> vec3f {
    let i = x + params.n.x * (y + params.n.y * z);
    return C[i].xyz - rest(i);
}
fn point(i: u32) {
    let p = P[i].xyz;
    let n = vec3f(params.n.xyz - vec3u(1u));
    let t = clamp((p - params.lo.xyz) / max(params.size.xyz, vec3f(1e-9)) * n, vec3f(0.0), n);
    let c = min(vec3u(floor(t)), params.n.xyz - vec3u(2u));
    let f = t - vec3f(c);
    let d00 = mix(disp(c.x, c.y, c.z), disp(c.x + 1u, c.y, c.z), f.x);
    let d10 = mix(disp(c.x, c.y + 1u, c.z), disp(c.x + 1u, c.y + 1u, c.z), f.x);
    let d01 = mix(disp(c.x, c.y, c.z + 1u), disp(c.x + 1u, c.y, c.z + 1u), f.x);
    let d11 = mix(disp(c.x, c.y + 1u, c.z + 1u), disp(c.x + 1u, c.y + 1u, c.z + 1u), f.x);
    let d = mix(mix(d00, d10, f.y), mix(d01, d11, f.y), f.z);
    P[i] = vec4f(p + d, P[i].w);
}
";

pub struct Lattice {
    cage: PointKernel,
    deform: PointKernel,
    /// The noise cage (up to 16³ control points).
    noise_cage: wgpu::Buffer,
    /// Bound as the cage kernel's header (it reads none).
    header: wgpu::Buffer,
    /// The lattice's edges, for the cage output, by divisions.
    edges: Option<([u32; 3], Topology, Attribute)>,
    names: [Arc<str>; 1],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    let max = MAX_DIVISIONS * MAX_DIVISIONS * MAX_DIVISIONS;
    Box::new(Lattice {
        cage: PointKernel::with_main(
            device,
            "pnt.lattice.cage",
            &format!("{COMMON}{CAGE}"),
            &[Bind::ReadWrite],
            80,
            CAGE_MAIN,
        ),
        deform: PointKernel::new(
            device,
            "pnt.lattice",
            &format!("{COMMON}{DEFORM}"),
            &[Bind::ReadWrite, Bind::Read],
            80,
        ),
        noise_cage: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.lattice.cage"),
            size: u64::from(max) * 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        header: kit::work_header(device),
        edges: None,
        names: [Arc::from(names::P)],
    })
}

/// Divisions from the parameter, each 2-16.
fn divisions(p: &Params<'_>) -> [u32; 3] {
    let d = p.vec4("divisions");
    [d[0], d[1], d[2]].map(|v| (v.round() as i64).clamp(2, i64::from(MAX_DIVISIONS)) as u32)
}

/// The lattice's edges as two-point lines, and the control points' ids.
fn edges(n: [u32; 3]) -> (Topology, Attribute) {
    let idx = |x: u32, y: u32, z: u32| x + n[0] * (y + n[1] * z);
    let mut lines: Vec<[u32; 2]> = Vec::new();
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                if x + 1 < n[0] {
                    lines.push([idx(x, y, z), idx(x + 1, y, z)]);
                }
                if y + 1 < n[1] {
                    lines.push([idx(x, y, z), idx(x, y + 1, z)]);
                }
                if z + 1 < n[2] {
                    lines.push([idx(x, y, z), idx(x, y, z + 1)]);
                }
            }
        }
    }
    let prims: Vec<(PrimKind, &[u32])> =
        lines.iter().map(|l| (PrimKind::Polyline, &l[..])).collect();
    let ids: Vec<u32> = (0..n[0] * n[1] * n[2]).collect();
    (
        Topology::from_prims(&prims),
        Attribute::from_u32(names::ID, &ids),
    )
}

impl GeoOp for Lattice {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let n = divisions(&p);
        let total = n[0] * n[1] * n[2];
        let size = vec3(&p, "size");
        let c = vec3(&p, "centre");
        let lo = [
            c[0] - size[0] * 0.5,
            c[1] - size[1] * 0.5,
            c[2] - size[2] * 0.5,
            0.0,
        ];
        let ev = p.f32("evolution");
        let mut u = Packer::<80>::new();
        u.vec4(lo)
            .vec4(size)
            .vec4([p.f32("amplitude"), p.f32("frequency").max(0.0), 0.0, 0.0])
            .vec4([ev * 0.31, -ev * 0.17, ev, 0.0])
            .u32(n[0])
            .u32(n[1])
            .u32(n[2])
            .u32(kit::seed(&p));
        // The displaced cage: the input's `P`, or noise.
        let cage = match ctx.input(1).filter(|g| g.points() > 0).cloned() {
            Some(g) => {
                if g.points() != total || g.is_gpu_counted() {
                    return Err(format!(
                        "the cage has {} points; Divisions {}×{}×{} needs {total}",
                        g.points(),
                        n[0],
                        n[1],
                        n[2]
                    ));
                }
                kit::readable(ctx, &g, names::P, AttrType::VEC3)?
            }
            None => {
                self.cage.write(ctx.queue, u.bytes());
                let device = ctx.device;
                let mut ps = pass(ctx.encoder);
                self.cage.dispatch_in(
                    device,
                    &mut ps,
                    &self.header,
                    &[&self.noise_cage],
                    Work::Elements(total),
                );
                drop(ps);
                self.noise_cage.clone()
            }
        };
        if menu_index(OUTPUTS, p.str("output")) == 1 {
            if self.edges.as_ref().is_none_or(|e| e.0 != n) {
                let (t, id) = edges(n);
                self.edges = Some((n, t, id));
            }
            let (_, topology, id) = self.edges.as_ref().expect("made above");
            let pos = ctx.alloc(self.names[0].clone(), AttrType::VEC3, total);
            ctx.encoder.copy_buffer_to_buffer(
                &cage,
                0,
                pos.gpu_buffer().expect("on the GPU"),
                0,
                u64::from(total) * 16,
            );
            let header = ctx.cpu_header(total);
            let out = &mut *ctx.output;
            out.clear();
            out.set_points(total, Some(header));
            out.set_point(pos);
            out.set_point(id.clone());
            out.set_topology(Some(topology.clone()));
            return Ok(());
        }
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
        if pos == cage {
            return Err("the cage cannot be the geometry it deforms".into());
        }
        self.deform.run(ctx, &hdr, &[&pos, &cage], u.bytes());
        *ctx.output = g;
        Ok(())
    }
}
