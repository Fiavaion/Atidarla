//! Spatial queries on the GPU grid hash of `neighbours.rs`: Attribute
//! Transfer (an attribute carried over from the nearest points of another
//! geometry), Proximity (distance and vector to the nearest point of
//! another geometry, or of the same one) and Neighbours (the k nearest
//! points within a radius).
//!
//! The grid is built over the geometry searched (the second input, or the
//! first when it is missing); the query runs over the first input's points
//! and dispatches from its header. A query visits points cell by cell in a
//! fixed order and keeps the nearest (ties go to the lower point number),
//! so results repeat exactly.

use std::sync::Arc;

use lsr_core::geo::{AttrType, Scalar, names};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params};
use lsr_gpu::geo::Geometry;
use lsr_gpu::geo::kernels::{Bind, Packer, Work, pass};

use crate::kit::{self, Missing, PointKernel, desc, menu_index, typed_access};
use crate::neighbours::{Grid, QUERY, QUERY_BINDINGS};

/// Most neighbours Neighbours keeps per point.
pub const MAX_K: u32 = 4;

/// `nbr`: a point's nearest neighbours as four signed integers.
const IVEC4: AttrType = AttrType::new(Scalar::I32, 4);

fn search_params(radius: f64, max: i64) -> [ParamSchema; 2] {
    [
        ParamSchema::float("radius", "Radius", radius, Some(1e-6), None)
            .help("Points farther than this are not found (also the grid's cell size)."),
        ParamSchema::int("max_neighbours", "Max Neighbours", max, Some(1), Some(256)).help(
            "A bound on the work per point: at most this many points within Radius are visited, cell by cell in a fixed order; where more are within Radius the nearest may be missed: raise it or lower Radius.",
        ),
    ]
}

fn max_neighbours(p: &Params<'_>) -> u32 {
    p.i64("max_neighbours").clamp(1, 256) as u32
}

/// The query input, or nothing to do.
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

/// Packs the grid words and a query's own words.
fn grid_uniforms<const N: usize>(built: &crate::neighbours::Built) -> Packer<N> {
    let mut u = Packer::<N>::new();
    u.vec4(built.grid);
    for w in built.gi {
        u.u32(w);
    }
    u
}

// --------------------------------------------------------- Attribute Transfer

const METHODS: &[&str] = &["nearest", "average"];

pub fn transfer_schema() -> OperatorSchema {
    let mut params = vec![
        ParamSchema::string("name", "Name", "Cd").help("The attribute carried from `from` (created on `in` with its type when missing)."),
        ParamSchema::menu("method", "Method", METHODS, "nearest").help("nearest: the value of the nearest point within Radius; average: the values of every point found within Radius, weighted by a smooth falloff of distance."),
    ];
    params.extend(search_params(0.25, 32));
    params.extend([
        ParamSchema::toggle("fade", "Fade", true).help("Blends by distance: full at the source point, nothing at Radius (off: full wherever a source is found)."),
        ParamSchema::float("amount", "Amount", 1.0, None, None).help("Blends from the point's own value (0) to the transferred one (1)."),
    ]);
    crate::pnt(
        "attribute_transfer",
        "Attribute Transfer",
        "Carries an attribute from the points of `from` to the nearby points of `in` (colours painted by moving brushes, masks from another cloud): the nearest source within Radius, or a distance-weighted average of every source within it, blended by distance and Amount. Points with no source within Radius keep their value. Searches a GPU grid hash.",
        &[("in", false), ("from", false)],
        params,
        900,
    )
}

const TRANSFER: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    ds: vec4u,
    dd: vec4u,
    // method, fade, the result is P, unused
    m: vec4u,
    // amount, radius, unused, unused
    k: vec4f,
};
@group(0) @binding(6) var<storage, read> Q: array<vec4f>;
@group(0) @binding(7) var<storage, read> S: array<u32>;
@group(0) @binding(8) var<storage, read_write> D: array<u32>;
var<private> best: u32;
var<private> bd: f32;
var<private> sum: vec4f;
var<private> wsum: f32;
fn visit(j: u32, d: vec3f, d2: f32) {
    if (d2 < bd) {
        bd = d2;
        best = j;
    }
    if (params.m.x == 1u) {
        let w = lsr_falloff(sqrt(d2), params.k.y) + 1e-6;
        sum = sum + ld_S(j) * w;
        wsum = wsum + w;
    }
}
fn point(i: u32) {
    var q = Q[i].xyz;
    if (params.m.z == 1u) { q = ld_D(i).xyz; }
    best = 0xffffffffu;
    bd = 3.4e38;
    sum = vec4f(0.0);
    wsum = 0.0;
    search(q);
    if (best == 0xffffffffu) { return; }
    var v = ld_S(best);
    if (params.m.x == 1u) { v = sum / wsum; }
    var w = params.k.x;
    if (params.m.y == 1u) { w = w * lsr_falloff(sqrt(bd), params.k.y); }
    let old = ld_D(i);
    st_D(i, mix(old, v, w));
}
";

pub struct Transfer {
    grid: Grid,
    kernel: PointKernel,
}

pub fn create_transfer(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Transfer {
        grid: Grid::new(device),
        kernel: PointKernel::new(
            device,
            "pnt.attribute_transfer",
            &format!(
                "{TRANSFER}{QUERY_BINDINGS}{}{}{QUERY}",
                typed_access("S", "params.ds", true),
                typed_access("D", "params.dd", false)
            ),
            &[Read, Read, Read, Read, Read, Read, ReadWrite],
            112,
        ),
    })
}

impl GeoOp for Transfer {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let Some(from) = ctx.input(1).filter(|f| f.points() > 0).cloned() else {
            *ctx.output = g;
            return Ok(());
        };
        let p = ctx.params;
        let name = p.str("name");
        let (src, src_ty) = kit::readable_any(ctx, &from, name)?;
        let from_p = kit::readable(ctx, &from, names::P, AttrType::VEC3)?;
        let radius = p.f32("radius").max(1e-6);
        let built = self
            .grid
            .build(ctx, &from, &from_p, radius, max_neighbours(&p))?;
        let self_p = name == names::P;
        let dst_ty = g.point(name).map_or(src_ty, |a| a.ty());
        let q = if self_p {
            None
        } else {
            Some(kit::readable(ctx, &g, names::P, AttrType::VEC3)?)
        };
        let hdr = kit::header_of(&g)?;
        let dst = kit::writable(ctx, &mut g, name, dst_ty, Missing::Zero)?;
        let q = q.unwrap_or_else(|| from_p.clone());
        let mut u = grid_uniforms::<112>(&built);
        for w in desc(src_ty).into_iter().chain(desc(dst_ty)) {
            u.u32(w);
        }
        u.u32(menu_index(METHODS, p.str("method")))
            .u32(u32::from(p.bool("fade")))
            .u32(u32::from(self_p))
            .u32(0)
            .vec4([p.f32("amount"), radius, 0.0, 0.0]);
        self.kernel.run(
            ctx,
            &hdr,
            &[
                &from_p,
                &built.vals,
                &built.start,
                &built.end,
                &q,
                &src,
                &dst,
            ],
            u.bytes(),
        );
        *ctx.output = g;
        Ok(())
    }
}

// ------------------------------------------------------------------ Proximity

pub fn proximity_schema() -> OperatorSchema {
    crate::pnt(
        "proximity",
        "Proximity",
        "For every point of `in`, the nearest point of `to` within Radius (of `in` itself, other than the point, when `to` is not connected): writes `dist` (its distance, or Radius when there is none) and `nearvec` (the vector to it, or zero). Drive colour, size or a force by how close things are. Searches a GPU grid hash.",
        &[("in", false), ("to", false)],
        search_params(0.5, 64).to_vec(),
        800,
    )
}

const PROXIMITY: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    // the query is the grid's own geometry
    own: vec4u,
};
@group(0) @binding(6) var<storage, read> Q: array<vec4f>;
@group(0) @binding(7) var<storage, read_write> DIST: array<f32>;
@group(0) @binding(8) var<storage, read_write> NV: array<vec4f>;
var<private> me: u32;
var<private> bd: f32;
var<private> bv: vec3f;
fn visit(j: u32, d: vec3f, d2: f32) {
    if (params.own.x == 1u && j == me) { return; }
    if (d2 < bd) {
        bd = d2;
        bv = d;
    }
}
fn point(i: u32) {
    me = i;
    bd = 3.4e38;
    bv = vec3f(0.0);
    search(Q[i].xyz);
    if (bd > 3.0e38) {
        DIST[i] = sqrt(params.grid.y);
        NV[i] = vec4f(0.0);
    } else {
        DIST[i] = sqrt(bd);
        NV[i] = vec4f(bv, 0.0);
    }
}
";

pub struct Proximity {
    grid: Grid,
    kernel: PointKernel,
    names: [Arc<str>; 2],
}

pub fn create_proximity(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Proximity {
        grid: Grid::new(device),
        kernel: PointKernel::new(
            device,
            "pnt.proximity",
            &format!("{PROXIMITY}{QUERY_BINDINGS}{QUERY}"),
            &[Read, Read, Read, Read, Read, ReadWrite, ReadWrite],
            48,
        ),
        names: ["dist", "nearvec"].map(Arc::from),
    })
}

impl GeoOp for Proximity {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let to = ctx.input(1).filter(|t| t.points() > 0).cloned();
        let own = to.is_none();
        let target = to.as_ref().unwrap_or(&g);
        let grid_p = kit::readable(ctx, target, names::P, AttrType::VEC3)?;
        let built = self
            .grid
            .build(ctx, target, &grid_p, p.f32("radius"), max_neighbours(&p))?;
        let q = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let hdr = kit::header_of(&g)?;
        let cap = g.points();
        let dist = ctx.alloc(self.names[0].clone(), AttrType::F32, cap);
        let nv = ctx.alloc(self.names[1].clone(), AttrType::VEC3, cap);
        let (db, nb) = (
            dist.gpu_buffer().expect("on the GPU").clone(),
            nv.gpu_buffer().expect("on the GPU").clone(),
        );
        let mut u = grid_uniforms::<48>(&built);
        u.u32(u32::from(own));
        self.kernel.write(ctx.queue, u.bytes());
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.kernel.dispatch_in(
            device,
            &mut ps,
            &hdr,
            &[&grid_p, &built.vals, &built.start, &built.end, &q, &db, &nb],
            Work::Header(&hdr),
        );
        drop(ps);
        g.set_point(dist);
        g.set_point(nv);
        *ctx.output = g;
        Ok(())
    }
}

// ----------------------------------------------------------------- Neighbours

pub fn neighbours_schema() -> OperatorSchema {
    let mut params = vec![
        ParamSchema::int("k", "K", 4, Some(1), Some(i64::from(MAX_K)))
            .help("How many nearest neighbours to keep (at most 4)."),
    ];
    params.extend(search_params(0.2, 64));
    crate::pnt(
        "neighbours",
        "Neighbours",
        "The k nearest neighbours of every point within Radius (k-NN, at most 4): writes `nbr` (four integers: their point numbers, nearest first, −1 where there are fewer), `nbrdist` (their distances, Radius for missing ones) and `nbrcount` (how many points are within Radius, up to Max Neighbours). Kernels and Attribute Math read them; Connect draws lines to them. Searches a GPU grid hash.",
        &[("in", false)],
        params,
        800,
    )
}

const NEIGHBOURS: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    // k, unused, unused, unused
    c: vec4u,
};
@group(0) @binding(6) var<storage, read_write> NBR: array<vec4i>;
@group(0) @binding(7) var<storage, read_write> ND: array<vec4f>;
@group(0) @binding(8) var<storage, read_write> NC: array<i32>;
var<private> me: u32;
var<private> count: i32;
var<private> bd: array<f32, 4>;
var<private> bj: array<u32, 4>;
fn visit(j: u32, d: vec3f, d2: f32) {
    if (j == me) { return; }
    count = count + 1;
    let k = params.c.x;
    if (d2 > bd[k - 1u] || (d2 == bd[k - 1u] && j >= bj[k - 1u])) { return; }
    var m = k - 1u;
    while (m > 0u && (d2 < bd[m - 1u] || (d2 == bd[m - 1u] && j < bj[m - 1u]))) {
        bd[m] = bd[m - 1u];
        bj[m] = bj[m - 1u];
        m = m - 1u;
    }
    bd[m] = d2;
    bj[m] = j;
}
fn point(i: u32) {
    me = i;
    count = 0;
    for (var m = 0u; m < 4u; m = m + 1u) {
        bd[m] = 3.4e38;
        bj[m] = 0xffffffffu;
    }
    search(P[i].xyz);
    let r = sqrt(params.grid.y);
    var n = vec4i(-1);
    var dd = vec4f(r);
    if (bj[0] != 0xffffffffu) { n.x = i32(bj[0]); dd.x = sqrt(bd[0]); }
    if (bj[1] != 0xffffffffu) { n.y = i32(bj[1]); dd.y = sqrt(bd[1]); }
    if (bj[2] != 0xffffffffu) { n.z = i32(bj[2]); dd.z = sqrt(bd[2]); }
    if (bj[3] != 0xffffffffu) { n.w = i32(bj[3]); dd.w = sqrt(bd[3]); }
    NBR[i] = n;
    ND[i] = dd;
    NC[i] = count;
}
";

pub struct Neighbours {
    grid: Grid,
    kernel: PointKernel,
    names: [Arc<str>; 3],
}

pub fn create_neighbours(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Neighbours {
        grid: Grid::new(device),
        kernel: PointKernel::new(
            device,
            "pnt.neighbours",
            &format!("{NEIGHBOURS}{QUERY_BINDINGS}{QUERY}"),
            &[Read, Read, Read, Read, ReadWrite, ReadWrite, ReadWrite],
            48,
        ),
        names: ["nbr", "nbrdist", "nbrcount"].map(Arc::from),
    })
}

impl GeoOp for Neighbours {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let built = self
            .grid
            .build(ctx, &g, &pos, p.f32("radius"), max_neighbours(&p))?;
        let hdr = kit::header_of(&g)?;
        let cap = g.points();
        let nbr = ctx.alloc(self.names[0].clone(), IVEC4, cap);
        let nd = ctx.alloc(self.names[1].clone(), AttrType::VEC4, cap);
        let nc = ctx.alloc(self.names[2].clone(), AttrType::I32, cap);
        let b = [&nbr, &nd, &nc].map(|a| a.gpu_buffer().expect("on the GPU").clone());
        let mut u = grid_uniforms::<48>(&built);
        u.u32(p.i64("k").clamp(1, i64::from(MAX_K)) as u32);
        self.kernel.write(ctx.queue, u.bytes());
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.kernel.dispatch_in(
            device,
            &mut ps,
            &hdr,
            &[
                &pos,
                &built.vals,
                &built.start,
                &built.end,
                &b[0],
                &b[1],
                &b[2],
            ],
            Work::Header(&hdr),
        );
        drop(ps);
        g.set_point(nbr);
        g.set_point(nd);
        g.set_point(nc);
        *ctx.output = g;
        Ok(())
    }
}
