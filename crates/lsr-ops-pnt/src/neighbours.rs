//! Neighbour operators on a GPU grid hash: Normals (a point cloud's surface
//! normals), Smooth (neighbour averaging of any attribute) and Connect
//! (lines to the nearest neighbours).
//!
//! **The grid.** Points are binned into cubic cells of the search radius;
//! each cell hashes into a table of the padded point count. A deterministic
//! sort of (cell hash, point number) ([`crate::sort::Sorter`]) lists the
//! points cell by cell, and a table kernel records where each hash's run
//! starts and ends. A query visits the 27 cells around a point, skips
//! points of other cells that share a hash, and keeps those within the
//! radius, in sorted order, so every result is the same on every run.
//! Max Neighbours bounds the work per point.

use std::sync::Arc;

use lsr_core::geo::{AttrType, names};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params};
use lsr_gpu::geo::kernels::{Bind, Packer, Work, pass};
use lsr_gpu::geo::{Attribute, Geometry, Topology};

use crate::kit::{self, PointKernel, desc, menu_index, typed_access, unit, vec3};
use crate::sort::{MAX_SORT, Sorter};

/// The query: `fn search(i, p)` calls `visit(j, d, d2)` for each point `j`
/// within the radius of `p` (including `i` itself), in a fixed order. Needs
/// `P`, `vals`, `cstart`, `cend` and `params.grid` (inverse cell size,
/// radius², unused, unused) and `params.gi` (table mask, most neighbours,
/// most candidates, unused).
pub(crate) const QUERY: &str = "fn cell_of(p: vec3f) -> vec3i {
    return vec3i(floor(p * params.grid.x));
}
fn cell_hash(c: vec3i) -> u32 {
    let u = bitcast<vec3u>(c);
    return lsr_hash(u.x * 73856093u ^ u.y * 19349663u ^ u.z * 83492791u) & params.gi.x;
}
fn search(p: vec3f) {
    let c = cell_of(p);
    var found = 0u;
    var seen = 0u;
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let cc = c + vec3i(dx, dy, dz);
                let h = cell_hash(cc);
                let e = cend[h];
                if (e == 0u) { continue; }
                for (var k = cstart[h]; k < e; k = k + 1u) {
                    seen = seen + 1u;
                    if (seen > params.gi.z) { return; }
                    let j = vals[k];
                    let q = P[j].xyz;
                    if (any(cell_of(q) != cc)) { continue; }
                    let d = q - p;
                    let d2 = dot(d, d);
                    if (d2 <= params.grid.y) {
                        visit(j, d, d2);
                        found = found + 1u;
                        if (found >= params.gi.y) { return; }
                    }
                }
            }
        }
    }
}
";

/// Bindings 2-5 of every query kernel.
pub(crate) const QUERY_BINDINGS: &str = "@group(0) @binding(2) var<storage, read> P: array<vec4f>;
@group(0) @binding(3) var<storage, read> vals: array<u32>;
@group(0) @binding(4) var<storage, read> cstart: array<u32>;
@group(0) @binding(5) var<storage, read> cend: array<u32>;
";

const CELLS: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    n2: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};
@group(0) @binding(2) var<storage, read> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> keys: array<u32>;
@group(0) @binding(4) var<storage, read_write> vals: array<u32>;
fn cell_of(p: vec3f) -> vec3i {
    return vec3i(floor(p * params.grid.x));
}
fn cell_hash(c: vec3i) -> u32 {
    let u = bitcast<vec3u>(c);
    return lsr_hash(u.x * 73856093u ^ u.y * 19349663u ^ u.z * 83492791u) & params.gi.x;
}
";

const CELLS_MAIN: &str = "if (i < params.n2) {
        var k = 0xffffffffu;
        if (i < hdr.count) { k = cell_hash(cell_of(P[i].xyz)); }
        keys[i] = k;
        vals[i] = i;
    }";

const TABLE: &str = "struct Params { pad: vec4u };
@group(0) @binding(2) var<storage, read> keys: array<u32>;
@group(0) @binding(3) var<storage, read_write> cstart: array<u32>;
@group(0) @binding(4) var<storage, read_write> cend: array<u32>;
fn point(j: u32) {
    let k = keys[j];
    if (j == 0u || keys[j - 1u] != k) { cstart[k] = j; }
    if (j + 1u == hdr.count || keys[j + 1u] != k) { cend[k] = j + 1u; }
}
";

/// The grid hash of one geometry: sort plus cell table.
pub(crate) struct Grid {
    sorter: Sorter,
    cells: PointKernel,
    table: PointKernel,
    /// Start and end per hash, sized to the padded count.
    tables: Option<(u32, wgpu::Buffer, wgpu::Buffer)>,
}

/// The buffers a query binds, and the uniform words it needs.
pub(crate) struct Built {
    pub vals: wgpu::Buffer,
    pub start: wgpu::Buffer,
    pub end: wgpu::Buffer,
    pub grid: [f32; 4],
    pub gi: [u32; 4],
}

impl Grid {
    pub fn new(device: &wgpu::Device) -> Self {
        use Bind::*;
        Self {
            sorter: Sorter::new(device),
            cells: PointKernel::with_main(
                device,
                "pnt.grid.cells",
                CELLS,
                &[Read, ReadWrite, ReadWrite],
                48,
                CELLS_MAIN,
            ),
            table: PointKernel::new(
                device,
                "pnt.grid.table",
                TABLE,
                &[Read, ReadWrite, ReadWrite],
                16,
            ),
            tables: None,
        }
    }

    /// Records the grid of `g`'s points for searches within `radius`
    /// (cells of that size), at most `max_neighbours` per point. The
    /// encoder gets a clear and one compute pass.
    pub fn build(
        &mut self,
        ctx: &mut GeoCtx<'_>,
        g: &Geometry,
        pos: &wgpu::Buffer,
        radius: f32,
        max_neighbours: u32,
    ) -> Result<Built, String> {
        let n = g.points();
        if n > MAX_SORT {
            return Err(format!(
                "neighbour searches handle at most {MAX_SORT} points"
            ));
        }
        let hdr = kit::header_of(g)?;
        let (keys, vals) = self.sorter.size(ctx.device, ctx.queue, n);
        let n2 = self.sorter.n2();
        if self.tables.as_ref().is_none_or(|t| t.0 != n2) {
            let table = |label: &str| {
                ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: u64::from(n2) * 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            };
            self.tables = Some((n2, table("pnt.grid.start"), table("pnt.grid.end")));
        }
        let (_, start, end) = self.tables.as_ref().expect("made above");
        let radius = radius.max(1e-6);
        let grid = [1.0 / radius, radius * radius, 0.0, 0.0];
        let max_neighbours = max_neighbours.max(1);
        let gi = [
            n2 - 1,
            max_neighbours,
            max_neighbours.saturating_mul(8).max(64),
            0,
        ];
        let mut u = Packer::<48>::new();
        u.vec4(grid);
        for w in gi {
            u.u32(w);
        }
        u.u32(n2);
        self.cells.write(ctx.queue, u.bytes());
        self.table.write(ctx.queue, &[0; 16]);
        ctx.encoder.clear_buffer(end, 0, None);
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.cells.dispatch_in(
            device,
            &mut ps,
            &hdr,
            &[pos, &keys, &vals],
            Work::Elements(n2),
        );
        self.sorter.record(&mut ps);
        self.table
            .run_in(device, &mut ps, &hdr, &[&keys, start, end]);
        drop(ps);
        Ok(Built {
            vals,
            start: start.clone(),
            end: end.clone(),
            grid,
            gi,
        })
    }
}

fn radius_params(radius: f64, max: i64) -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("radius", "Radius", radius, Some(1e-6), None)
            .help("Points within this distance are neighbours (also the grid's cell size)."),
        ParamSchema::int("max_neighbours", "Max Neighbours", max, Some(1), Some(256))
            .help("A bound on the work per point: at most this many neighbours are used. Where more are within Radius the first found (in a fixed order, cell by cell) are used, which biases the result: raise it or lower Radius."),
    ]
}

/// The input, or nothing to do.
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

fn max_neighbours(p: &Params<'_>) -> u32 {
    p.i64("max_neighbours").clamp(1, 256) as u32
}

// ------------------------------------------------------------------ Normals

const ORIENTS: &[&str] = &["outward", "toward", "none"];

pub fn normals_schema() -> OperatorSchema {
    let mut params = radius_params(0.2, 128);
    params.extend([
        ParamSchema::menu("orient", "Orient", ORIENTS, "outward").help(
            "outward: normals point away from Centre (closed shapes around it); toward: they point along Direction (open sheets); none: as computed (either side).",
        ),
        ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("outward: the point inside the shape."),
        ParamSchema::vec("direction", "Direction", &[0.0, 0.0, 1.0]).help("toward: the side normals face; also the normal of a point with fewer than three neighbours."),
    ]);
    crate::pnt(
        "normals",
        "Normals",
        "Estimates surface normals `N` for a point cloud: the direction in which each point's neighbours (within Radius) spread least, from their covariance, turned outward from a centre or toward a direction. Neighbours come from a GPU grid hash.",
        &[("in", false)],
        params,
        800,
    )
}

const NORMALS: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    centre: vec4f,
    dir: vec4f,
    orient: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};
@group(0) @binding(6) var<storage, read_write> N: array<vec4f>;
var<private> cnt: f32;
var<private> s1: vec3f;
var<private> sxx: vec3f;
var<private> sxy: vec3f;
fn visit(j: u32, d: vec3f, d2: f32) {
    cnt = cnt + 1.0;
    s1 = s1 + d;
    sxx = sxx + d * d;
    sxy = sxy + vec3f(d.x * d.y, d.y * d.z, d.x * d.z);
}
fn point(i: u32) {
    let p = P[i].xyz;
    cnt = 0.0;
    s1 = vec3f(0.0);
    sxx = vec3f(0.0);
    sxy = vec3f(0.0);
    search(p);
    var n = params.dir.xyz;
    if (cnt >= 3.0) {
        let m = s1 / cnt;
        let cxx = sxx / cnt - m * m;
        let cxy = sxy / cnt - vec3f(m.x * m.y, m.y * m.z, m.x * m.z);
        let C = mat3x3f(
            vec3f(cxx.x, cxy.x, cxy.z),
            vec3f(cxy.x, cxx.y, cxy.y),
            vec3f(cxy.z, cxy.y, cxx.z),
        );
        // The smallest eigenvalue in closed form (symmetric 3 × 3), then
        // its eigenvector as the longest cross product of two rows of
        // C − λ I (exact for points on a plane).
        let q = (cxx.x + cxx.y + cxx.z) / 3.0;
        let p1 = dot(cxy, cxy);
        let dq = cxx - vec3f(q);
        let p = sqrt((dot(dq, dq) + 2.0 * p1) / 6.0);
        if (p > 1e-20) {
            let I = mat3x3f(vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), vec3f(0.0, 0.0, 1.0));
            let B = (C - I * q) * (1.0 / p);
            let r = clamp(determinant(B) * 0.5, -1.0, 1.0);
            let phi = acos(r) / 3.0;
            let lmin = q + 2.0 * p * cos(phi + 2.0943951);
            let A = C - I * lmin;
            let c0 = cross(A[0], A[1]);
            let c1 = cross(A[0], A[2]);
            let c2 = cross(A[1], A[2]);
            var v = c0;
            if (dot(c1, c1) > dot(v, v)) { v = c1; }
            if (dot(c2, c2) > dot(v, v)) { v = c2; }
            let l = length(v);
            if (l > 1e-30) { n = v / l; }
        }
    }
    if (params.orient == 0u && dot(n, p - params.centre.xyz) < 0.0) { n = -n; }
    if (params.orient == 1u && dot(n, params.dir.xyz) < 0.0) { n = -n; }
    N[i] = vec4f(n, 0.0);
}
";

pub struct Normals {
    grid: Grid,
    kernel: PointKernel,
    name: Arc<str>,
}

pub fn create_normals(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Normals {
        grid: Grid::new(device),
        kernel: PointKernel::new(
            device,
            "pnt.normals",
            &format!("{NORMALS}{QUERY_BINDINGS}{QUERY}"),
            &[Read, Read, Read, Read, ReadWrite],
            80,
        ),
        name: Arc::from(names::N),
    })
}

impl GeoOp for Normals {
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
        let n = ctx.alloc(self.name.clone(), AttrType::VEC3, g.points());
        let nb = n.gpu_buffer().expect("on the GPU").clone();
        g.set_point(n);
        let mut u = Packer::<80>::new();
        u.vec4(built.grid);
        for w in built.gi {
            u.u32(w);
        }
        u.vec4(vec3(&p, "centre"))
            .vec4(unit(&p, "direction"))
            .u32(menu_index(ORIENTS, p.str("orient")));
        self.kernel.run(
            ctx,
            &hdr,
            &[&pos, &built.vals, &built.start, &built.end, &nb],
            u.bytes(),
        );
        *ctx.output = g;
        Ok(())
    }
}

// ------------------------------------------------------------------- Smooth

pub fn smooth_schema() -> OperatorSchema {
    let mut params = vec![
        ParamSchema::string("attribute", "Attribute", "P")
            .help("The point attribute smoothed (any type)."),
    ];
    params.extend(radius_params(0.2, 128));
    params.extend([
        ParamSchema::float("strength", "Strength", 1.0, Some(0.0), Some(1.0))
            .help("How far each value moves toward its neighbourhood's average per iteration."),
        ParamSchema::int("iterations", "Iterations", 1, Some(1), Some(16))
            .help("Repeats the averaging (the neighbourhoods stay those of the input positions)."),
    ]);
    crate::pnt(
        "smooth",
        "Smooth",
        "Averages a point attribute with its neighbours within Radius, weighted by distance (closer counts more): relaxes positions, blurs colours or any other attribute over a point cloud. Neighbours come from a GPU grid hash of the input positions.",
        &[("in", false)],
        params,
        800,
    )
}

const SMOOTH: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    d: vec4u,
    // strength, unused...
    s: vec4f,
};
@group(0) @binding(6) var<storage, read> S: array<u32>;
@group(0) @binding(7) var<storage, read_write> D: array<u32>;
var<private> acc: vec4f;
var<private> wsum: f32;
fn visit(j: u32, d: vec3f, d2: f32) {
    let t = 1.0 - d2 / params.grid.y;
    let w = t * t;
    acc = acc + ld_S(j) * w;
    wsum = wsum + w;
}
fn point(i: u32) {
    acc = vec4f(0.0);
    wsum = 0.0;
    search(P[i].xyz);
    let own = ld_S(i);
    var v = own;
    if (wsum > 0.0) { v = mix(own, acc / wsum, params.s.x); }
    st_D(i, v);
}
";

pub struct Smooth {
    grid: Grid,
    kernel: PointKernel,
}

pub fn create_smooth(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Smooth {
        grid: Grid::new(device),
        kernel: PointKernel::new(
            device,
            "pnt.smooth",
            &format!(
                "{SMOOTH}{QUERY_BINDINGS}{}{}{QUERY}",
                typed_access("S", "params.d", true),
                typed_access("D", "params.d", false)
            ),
            &[Read, Read, Read, Read, Read, ReadWrite],
            64,
        ),
    })
}

impl GeoOp for Smooth {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let name = p.str("attribute");
        let (src, ty) = kit::readable_any(ctx, &g, name)?;
        let arc = g.point(name).expect("read above").name_arc().clone();
        let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let built = self
            .grid
            .build(ctx, &g, &pos, p.f32("radius"), max_neighbours(&p))?;
        let hdr = kit::header_of(&g)?;
        let iterations = p.i64("iterations").clamp(1, 16) as u32;
        let a = ctx.alloc(arc.clone(), ty, g.points());
        let b = (iterations > 1).then(|| ctx.alloc(arc, ty, g.points()));
        let mut u = Packer::<64>::new();
        u.vec4(built.grid);
        for w in built.gi.into_iter().chain(desc(ty)) {
            u.u32(w);
        }
        u.vec4([p.f32("strength").clamp(0.0, 1.0), 0.0, 0.0, 0.0]);
        self.kernel.write(ctx.queue, u.bytes());
        let ab = a.gpu_buffer().expect("on the GPU").clone();
        let bb = b
            .as_ref()
            .map(|b| b.gpu_buffer().expect("on the GPU").clone());
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        let mut last_a = true;
        for k in 0..iterations {
            let (s, d) = match (k, &bb) {
                (0, _) => (&src, &ab),
                (k, Some(bb)) if k % 2 == 1 => (&ab, bb),
                (_, Some(bb)) => (bb, &ab),
                _ => unreachable!("a second buffer when iterating"),
            };
            last_a = std::ptr::eq(d, &ab);
            self.kernel.run_in(
                device,
                &mut ps,
                &hdr,
                &[&pos, &built.vals, &built.start, &built.end, s, d],
            );
        }
        drop(ps);
        let result: Attribute = match (last_a, b) {
            (false, Some(b)) => b,
            _ => a,
        };
        g.set_point(result);
        *ctx.output = g;
        Ok(())
    }
}

// ------------------------------------------------------------------ Connect

/// Most lines per point.
pub const MAX_LINKS: u32 = 8;

pub fn connect_schema() -> OperatorSchema {
    let mut params = radius_params(0.15, 64);
    params.extend([
        ParamSchema::int("links", "Links", 2, Some(1), Some(i64::from(MAX_LINKS)))
            .help("Lines from each point to its nearest neighbours."),
        ParamSchema::toggle("unique", "Unique", true).help(
            "Links a pair once: each point links only to neighbours with a higher point number.",
        ),
    ]);
    crate::pnt(
        "connect",
        "Connect",
        "Links each point to its nearest neighbours within Radius with lines (polyline primitives of two points; Render Points draws them with Draw set to lines), and writes `near` (the nearest neighbour's number, or the point's own) and `neardist` (its distance, or Radius when there is none). Neighbours come from a GPU grid hash.",
        &[("in", false)],
        params,
        800,
    )
}

const CONNECT: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    // links, unique, capacity, unused
    c: vec4u,
};
@group(0) @binding(6) var<storage, read_write> corners: array<u32>;
@group(0) @binding(7) var<storage, read_write> near: array<u32>;
@group(0) @binding(8) var<storage, read_write> neardist: array<f32>;
var<private> me: u32;
var<private> bd: array<f32, 8>;
var<private> bj: array<u32, 8>;
fn visit(j: u32, d: vec3f, d2: f32) {
    if (j == me || (params.c.y == 1u && j < me)) { return; }
    // Insert (d2, j) into the sorted best list.
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
    let k = params.c.x;
    for (var m = 0u; m < 8u; m = m + 1u) {
        bd[m] = 3.4e38;
        bj[m] = 0xffffffffu;
    }
    if (i < hdr.count) { search(P[i].xyz); }
    for (var m = 0u; m < k; m = m + 1u) {
        let base = (i * k + m) * 2u;
        var other = i;
        if (bj[m] != 0xffffffffu) { other = bj[m]; }
        if (i >= hdr.count) { other = i; }
        corners[base] = i;
        corners[base + 1u] = other;
    }
    if (bj[0] != 0xffffffffu) {
        near[i] = bj[0];
        neardist[i] = sqrt(bd[0]);
    } else {
        near[i] = i;
        neardist[i] = sqrt(params.grid.y);
    }
}
";

const TOPOLOGY: &str = "struct Params { prims: vec4u };
@group(0) @binding(2) var<storage, read_write> kinds: array<u32>;
@group(0) @binding(3) var<storage, read_write> starts: array<u32>;
";

const TOPOLOGY_MAIN: &str = "if (i < params.prims.x) {
        kinds[i] = 1u;
        starts[i] = i * 2u;
    }
    if (i == params.prims.x) { starts[i] = i * 2u; }";

pub struct Connect {
    grid: Grid,
    kernel: PointKernel,
    topology: PointKernel,
    names: [Arc<str>; 5],
}

pub fn create_connect(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Connect {
        grid: Grid::new(device),
        kernel: PointKernel::with_main(
            device,
            "pnt.connect",
            &format!("{CONNECT}{QUERY_BINDINGS}{QUERY}"),
            &[Read, Read, Read, Read, ReadWrite, ReadWrite, ReadWrite],
            48,
            "if (i < params.c.z) { point(i); }",
        ),
        topology: PointKernel::with_main(
            device,
            "pnt.connect.topology",
            TOPOLOGY,
            &[ReadWrite, ReadWrite],
            16,
            TOPOLOGY_MAIN,
        ),
        names: ["__kind", "__start", "__corners", "near", "neardist"].map(Arc::from),
    })
}

impl GeoOp for Connect {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let links = p.i64("links").clamp(1, i64::from(MAX_LINKS)) as u32;
        let cap = g.points();
        let prims = u64::from(cap) * u64::from(links);
        if prims * 2 > ctx.device.limits().max_storage_buffer_binding_size / 4 {
            return Err(format!("{prims} lines are more than this GPU can hold"));
        }
        let prims = prims as u32;
        let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let radius = p.f32("radius");
        let built = self.grid.build(ctx, &g, &pos, radius, max_neighbours(&p))?;
        let hdr = kit::header_of(&g)?;
        let [kind_n, start_n, corner_n, near_n, dist_n] = self.names.clone();
        let kinds = ctx.alloc(kind_n, AttrType::U32, prims);
        let starts = ctx.alloc(start_n, AttrType::U32, prims + 1);
        let corners = ctx.alloc(corner_n, AttrType::U32, prims * 2);
        let near = ctx.alloc(near_n, AttrType::U32, cap);
        let dist = ctx.alloc(dist_n, AttrType::F32, cap);
        let buf = |a: &Attribute| a.gpu_buffer().expect("on the GPU").clone();
        let (kb, sb, cb, nb, db) = (
            buf(&kinds),
            buf(&starts),
            buf(&corners),
            buf(&near),
            buf(&dist),
        );
        let mut u = Packer::<48>::new();
        u.vec4(built.grid);
        for w in built
            .gi
            .into_iter()
            .chain([links, u32::from(p.bool("unique")), cap, 0])
        {
            u.u32(w);
        }
        self.kernel.write(ctx.queue, u.bytes());
        self.topology
            .write(ctx.queue, &lsr_gpu::geo::kernels::words([prims, 0, 0, 0]));
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.kernel.dispatch_in(
            device,
            &mut ps,
            &hdr,
            &[&pos, &built.vals, &built.start, &built.end, &cb, &nb, &db],
            Work::Elements(cap),
        );
        self.topology.dispatch_in(
            device,
            &mut ps,
            &hdr,
            &[&kb, &sb],
            Work::Elements(prims + 1),
        );
        drop(ps);
        g.set_point(near);
        g.set_point(dist);
        // Earlier primitives' attributes would not match the new ones.
        crate::select::drop_prims(&mut g);
        g.set_topology(Some(Topology {
            kinds,
            start: starts,
            corners,
        }));
        *ctx.output = g;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(src: &str) {
        let full = format!(
            "{}{src}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@compute @workgroup_size(256) fn main(@builtin(global_invocation_id) gid: vec3u) {{ point(gid.x); }}",
            kit::prelude()
        );
        let module = naga::front::wgsl::parse_str(&full)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&full)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn neighbour_kernels_are_valid_wgsl() {
        validate(&format!("{NORMALS}{QUERY_BINDINGS}{QUERY}"));
        validate(&format!(
            "{SMOOTH}{QUERY_BINDINGS}{}{}{QUERY}",
            typed_access("S", "params.d", true),
            typed_access("D", "params.d", false)
        ));
        validate(&format!("{CONNECT}{QUERY_BINDINGS}{QUERY}"));
        validate(TABLE);
    }
}
