//! Ray Intersect: casts a ray from every point against the polygons of a
//! target geometry (an MSH mesh, or any geometry with polygon primitives)
//! and records the nearest hit, optionally moving the point onto it
//! (projecting points onto a surface, shrink-wrapping, "hit the floor").
//!
//! Brute force on the GPU: each point tests every triangle of the target's
//! polygons (fans of their corners; polylines are skipped), so the cost is
//! points × triangles; meant for targets of up to a few thousand triangles.
//! Möller–Trumbore intersection; ties go to the lower primitive, so the
//! result repeats exactly.

use std::sync::Arc;

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{Bind, Packer};

use crate::kit::{self, Missing, PointKernel, menu_index, unit, vec3};

const DIRECTIONS: &[&str] = &["vector", "normal", "point"];

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "ray_intersect",
        "Ray Intersect",
        "Casts a ray from every point of `in` (along Direction, along the point's normal `N`, or toward Centre) against the polygons of `target` and keeps the nearest hit within Max Distance: writes `hitdist` (the distance, or −1 for a miss) and, with Project on, moves the point onto the surface. Both Ways also casts backwards. Brute force on the GPU (points × triangles): for targets up to a few thousand triangles.",
        &[("in", false), ("target", false)],
        vec![
            ParamSchema::menu("direction_from", "Direction From", DIRECTIONS, "vector").help("vector: Direction for every point; normal: each point's `N`; point: toward Centre."),
            ParamSchema::vec("direction", "Direction", &[0.0, -1.0, 0.0]).help("vector: the ray direction."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("point: where the rays aim."),
            ParamSchema::float("max_distance", "Max Distance", 100.0, Some(0.0), None).help("Hits farther than this are ignored."),
            ParamSchema::toggle("both_ways", "Both Ways", false).help("Also casts in the opposite direction; the nearer hit wins."),
            ParamSchema::toggle("project", "Project", true).help("Moves each point that hits onto the surface."),
        ],
        500,
    )
}

const RAY: &str = "struct Params {
    dir: vec4f,
    centre: vec4f,
    // max distance, unused, unused, unused
    m: vec4f,
    // direction from, both ways, project, primitives
    f: vec4u,
};
@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read> N: array<vec4f>;
@group(0) @binding(4) var<storage, read> TP: array<vec4f>;
@group(0) @binding(5) var<storage, read> KIND: array<u32>;
@group(0) @binding(6) var<storage, read> START: array<u32>;
@group(0) @binding(7) var<storage, read> CORNER: array<u32>;
@group(0) @binding(8) var<storage, read_write> HD: array<f32>;
fn tri(o: vec3f, d: vec3f, a: vec3f, b: vec3f, c: vec3f) -> f32 {
    let e1 = b - a;
    let e2 = c - a;
    let h = cross(d, e2);
    let det = dot(e1, h);
    if (abs(det) < 1e-12) { return -1.0; }
    let inv = 1.0 / det;
    let s = o - a;
    let u = dot(s, h) * inv;
    if (u < 0.0 || u > 1.0) { return -1.0; }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if (v < 0.0 || u + v > 1.0) { return -1.0; }
    return dot(e2, q) * inv;
}
fn point(i: u32) {
    let o = P[i].xyz;
    var d = params.dir.xyz;
    if (params.f.x == 1u) { d = N[i].xyz; }
    if (params.f.x == 2u) { d = params.centre.xyz - o; }
    let len = length(d);
    if (len < 1e-12) { HD[i] = -1.0; return; }
    d = d / len;
    var best = params.m.x;
    var hit = false;
    var sgn = 1.0;
    for (var p = 0u; p < params.f.w; p = p + 1u) {
        if (KIND[p] != 0u) { continue; }
        let s = START[p];
        let e = START[p + 1u];
        if (e < s + 3u) { continue; }
        let a = TP[CORNER[s]].xyz;
        for (var k = s + 1u; k + 1u < e; k = k + 1u) {
            let b = TP[CORNER[k]].xyz;
            let c = TP[CORNER[k + 1u]].xyz;
            let t = tri(o, d, a, b, c);
            if (t >= 0.0 && t < best) { best = t; hit = true; sgn = 1.0; }
            if (params.f.y == 1u) {
                let tb = tri(o, -d, a, b, c);
                if (tb >= 0.0 && tb < best) { best = tb; hit = true; sgn = -1.0; }
            }
        }
    }
    if (hit) {
        HD[i] = best;
        if (params.f.z == 1u) { P[i] = vec4f(o + d * best * sgn, P[i].w); }
    } else {
        HD[i] = -1.0;
    }
}
";

pub struct RayIntersect {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
    names: [Arc<str>; 1],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(RayIntersect {
        kernel: PointKernel::new(
            device,
            "pnt.ray_intersect",
            RAY,
            &[ReadWrite, Read, Read, Read, Read, Read, ReadWrite],
            64,
        ),
        dummy: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.ray_intersect.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
        names: [Arc::from("hitdist")],
    })
}

impl GeoOp for RayIntersect {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let p = ctx.params;
        let target = ctx.input(1).cloned();
        let Some((target, topology)) = target.and_then(|t| {
            let topo = t.topology().cloned()?;
            Some((t, topo))
        }) else {
            *ctx.output = g;
            return Ok(());
        };
        let from = menu_index(DIRECTIONS, p.str("direction_from"));
        let tp = kit::readable(ctx, &target, names::P, AttrType::VEC3)?;
        let [kinds, start, corners] = [&topology.kinds, &topology.start, &topology.corners]
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        let n = if from == 1 {
            kit::readable(ctx, &g, names::N, AttrType::VEC3)?
        } else {
            self.dummy.clone()
        };
        let hdr = kit::header_of(&g)?;
        let pos = kit::writable(ctx, &mut g, names::P, AttrType::VEC3, Missing::Error)?;
        if pos == tp {
            return Err("the target cannot be the points it is cast from".into());
        }
        let cap = g.points();
        let hd = ctx.alloc(self.names[0].clone(), AttrType::F32, cap);
        let hb = hd.gpu_buffer().expect("on the GPU").clone();
        let mut u = Packer::<64>::new();
        u.vec4(if from == 0 {
            unit(&p, "direction")
        } else {
            [0.0; 4]
        })
        .vec4(vec3(&p, "centre"))
        .vec4([p.f32("max_distance").max(0.0), 0.0, 0.0, 0.0])
        .u32(from)
        .u32(u32::from(p.bool("both_ways")))
        .u32(u32::from(p.bool("project")))
        .u32(topology.len());
        self.kernel.run(
            ctx,
            &hdr,
            &[&pos, &n, &tp, &kinds, &start, &corners, &hb],
            u.bytes(),
        );
        g.set_point(hd);
        *ctx.output = g;
        Ok(())
    }
}
