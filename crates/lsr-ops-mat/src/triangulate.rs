//! Turns a geometry's primitives (CSR topology, design D1) into draw
//! records on the GPU, for `img.render`:
//!
//! - every polygon of `n ≥ 3` corners becomes a fan of `n − 2` triangles,
//!   each a `vec4u(a, b, c, prim | edges << 29)` where bit `i` of `edges`
//!   says whether the side opposite corner `i` is a side of the polygon (not
//!   a diagonal of the fan), for wireframes;
//! - every polyline of `n ≥ 2` points becomes `n − 1` segments
//!   `vec4u(a, b, prim, 0)`.
//!
//! Passes: count per primitive, an exclusive [`Scan`] of each count, a
//! header per record list (count, `draw_indirect` and dispatch arguments),
//! then one invocation per record finds its primitive by binary search over
//! the scan (no loop over a primitive's corners, so one huge polygon does
//! not stall a thread). Integer only, no atomics: the same topology gives
//! the same records on every run.
//!
//! **Hostile topology is made harmless here:** sizes are checked on the
//! CPU (the `start` list must have one entry more than there are
//! primitives; record lists must fit the device), a primitive whose range
//! runs backwards or past the corner list is skipped, a corner index at or
//! beyond the point count makes its record degenerate (nothing is drawn),
//! and the record counts are clamped to the buffers' capacity, so no
//! shader reads or writes out of bounds. Records are rebuilt only when the
//! topology or the point count changes.

use lsr_core::geo::{WORKGROUP, header};
use lsr_gpu::geo::kernels::{Bind, Kernel, Scan, Uniforms, Work, pass, words};
use lsr_gpu::geo::{BufferPool, GeoBuffer, Geometry, Topology};

/// Bytes of one record (`vec4u`).
pub const RECORD: u64 = 16;
/// Most primitives per geometry: IDs share a word with three edge bits,
/// and the per-primitive passes dispatch at most 32,768 workgroups.
pub const MAX_PRIMS: u32 = 1 << 23;

const COMMON: &str = r#"
struct TP { prims: u32, corners: u32, points: u32, scan_n: u32, tri_cap: u32, seg_cap: u32, pad0: u32, pad1: u32 };
@group(0) @binding(0) var<uniform> tp: TP;

// The corner range of primitive p, or an empty one when it is malformed.
fn span(p: u32) -> vec2u {
    let s = start[p];
    let e = start[p + 1u];
    if (e < s || e > tp.corners) { return vec2u(0u, 0u); }
    return vec2u(s, e - s);
}
"#;

const COUNT: &str = r#"
@group(0) @binding(1) var<storage, read> kinds: array<u32>;
@group(0) @binding(2) var<storage, read> start: array<u32>;
@group(0) @binding(3) var<storage, read_write> tri_n: array<u32>;
@group(0) @binding(4) var<storage, read_write> seg_n: array<u32>;

@compute @workgroup_size(256)
fn count(@builtin(global_invocation_id) gid: vec3u) {
    let p = gid.x;
    if (p >= tp.scan_n) { return; }
    var t = 0u;
    var s = 0u;
    if (p < tp.prims) {
        let n = span(p).y;
        let kind = kinds[p];
        if (kind == 0u && n >= 3u) { t = n - 2u; }
        if (kind == 1u && n >= 2u) { s = n - 1u; }
    }
    tri_n[p] = t;
    seg_n[p] = s;
}
"#;

const FINISH: &str = r#"
@group(0) @binding(1) var<storage, read> tri_total: array<u32>;
@group(0) @binding(2) var<storage, read> seg_total: array<u32>;
@group(0) @binding(3) var<storage, read_write> tri_hdr: GeoHeader;
@group(0) @binding(4) var<storage, read_write> seg_hdr: GeoHeader;

fn fill(n: u32, cap: u32, verts: u32) -> GeoHeader {
    var h: GeoHeader;
    let c = min(n, cap);
    h.count = c;
    h.capacity = cap;
    h.mapped = c;
    h.pad = 0u;
    h.draw[0] = c * verts;
    h.draw[1] = 1u;
    h.draw[2] = 0u;
    h.draw[3] = 0u;
    h.dispatch[0] = max(1u, (c + 255u) / 256u);
    h.dispatch[1] = 1u;
    h.dispatch[2] = 1u;
    h.dispatch[3] = 0u;
    return h;
}

@compute @workgroup_size(1)
fn finish() {
    tri_hdr = fill(tri_total[0], tp.tri_cap, 3u);
    seg_hdr = fill(seg_total[0], tp.seg_cap, 6u);
}
"#;

const EMIT: &str = r#"
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@group(0) @binding(2) var<storage, read> off: array<u32>;
@group(0) @binding(3) var<storage, read> start: array<u32>;
@group(0) @binding(4) var<storage, read> corners: array<u32>;
@group(0) @binding(5) var<storage, read_write> out: array<vec4u>;

// The primitive whose records include record r: the last p with off[p] <= r
// (primitives with no records share the next one's offset).
fn owner(r: u32) -> u32 {
    var lo = 0u;
    var hi = tp.prims;
    while (hi - lo > 1u) {
        let mid = (lo + hi) / 2u;
        if (off[mid] <= r) { lo = mid; } else { hi = mid; }
    }
    return lo;
}

fn corner_point(c: u32) -> u32 { return corners[c]; }
fn ok(i: u32) -> bool { return i < tp.points; }

@compute @workgroup_size(256)
fn emit_tri(@builtin(global_invocation_id) gid: vec3u) {
    let r = gid.x;
    if (r >= hdr.count) { return; }
    let p = owner(r);
    let sp = span(p);
    let k = r - off[p];
    let n = sp.y;
    if (n < 3u || k + 2u >= n) { out[r] = vec4u(0u, 0u, 0u, p); return; }
    let a = corner_point(sp.x);
    let b = corner_point(sp.x + k + 1u);
    let c = corner_point(sp.x + k + 2u);
    var edges = 1u;
    if (k + 3u == n) { edges = edges | 2u; }
    if (k == 0u) { edges = edges | 4u; }
    if (ok(a) && ok(b) && ok(c)) {
        out[r] = vec4u(a, b, c, p | (edges << 29u));
    } else {
        out[r] = vec4u(0u, 0u, 0u, p);
    }
}

@compute @workgroup_size(256)
fn emit_seg(@builtin(global_invocation_id) gid: vec3u) {
    let r = gid.x;
    if (r >= hdr.count) { return; }
    let p = owner(r);
    let sp = span(p);
    let k = r - off[p];
    if (sp.y < 2u || k + 1u >= sp.y) { out[r] = vec4u(0u, 0u, p, 0u); return; }
    let a = corner_point(sp.x + k);
    let b = corner_point(sp.x + k + 1u);
    if (ok(a) && ok(b)) {
        out[r] = vec4u(a, b, p, 0u);
    } else {
        out[r] = vec4u(0u, 0u, p, 0u);
    }
}
"#;

/// Upper bounds of the record counts, exact when the topology is on the
/// CPU (the same formula the GPU uses), else the corner count for both.
pub fn bounds(topo: &Topology) -> (u64, u64) {
    let corners = u64::from(topo.corners.len());
    let (Some(kinds), Some(start)) = (topo.kinds.cpu_bytes(), topo.start.cpu_bytes()) else {
        return (corners, corners);
    };
    let word = |b: &[u8], i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    let (mut tris, mut segs) = (0u64, 0u64);
    for p in 0..topo.len() as usize {
        let (s, e) = (word(start, p), word(start, p + 1));
        if e < s || u64::from(e) > corners {
            continue;
        }
        let n = u64::from(e - s);
        match word(kinds, p) {
            0 if n >= 3 => tris += n - 2,
            1 if n >= 2 => segs += n - 1,
            _ => {}
        }
    }
    (tris, segs)
}

/// The most records a list may hold on `device`: one storage binding, and
/// one-dimensional dispatches of at most 65,535 workgroups.
pub fn max_records(limits: &wgpu::Limits) -> u64 {
    let binding = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size);
    (binding / RECORD).min(65_535 * u64::from(WORKGROUP))
}

/// Checks a topology before anything reaches the GPU.
pub fn validate(topo: &Topology, limits: &wgpu::Limits) -> Result<(u64, u64), String> {
    let prims = topo.len();
    if topo.start.len() != prims.saturating_add(1) {
        return Err(format!(
            "malformed topology: {} primitives need {} start offsets, it has {}",
            prims,
            u64::from(prims) + 1,
            topo.start.len()
        ));
    }
    if prims >= MAX_PRIMS {
        return Err(format!(
            "{prims} primitives: the renderer draws at most {MAX_PRIMS}"
        ));
    }
    let (tris, segs) = bounds(topo);
    let max = max_records(limits);
    if tris > max || segs > max {
        return Err(format!(
            "{tris} triangles and {segs} segments: this device draws at most {max} of each in one geometry"
        ));
    }
    Ok((tris, segs))
}

/// The draw records of one geometry (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct Records<'a> {
    pub tris: &'a wgpu::Buffer,
    /// A geometry header: `draw_indirect` arguments at
    /// [`header::DRAW_OFFSET`] (3 vertices per triangle).
    pub tri_header: &'a wgpu::Buffer,
    pub segs: &'a wgpu::Buffer,
    /// As `tri_header`, 6 vertices per segment.
    pub seg_header: &'a wgpu::Buffer,
    /// Whether there can be any triangles or segments at all.
    pub any_tris: bool,
    pub any_segs: bool,
}

#[derive(Debug)]
struct Lists {
    scan_n: u32,
    tri_scan: Scan,
    seg_scan: Scan,
    tri_n: GeoBuffer,
    seg_n: GeoBuffer,
    tri_off: GeoBuffer,
    seg_off: GeoBuffer,
}

/// GPU triangulation with its buffers (one per renderer slot).
#[derive(Debug)]
pub struct Triangulator {
    count: Kernel,
    finish: Kernel,
    emit_tri: Kernel,
    emit_seg: Kernel,
    params: Uniforms,
    lists: Option<Lists>,
    tri_header: wgpu::Buffer,
    seg_header: wgpu::Buffer,
    tris: Option<GeoBuffer>,
    segs: Option<GeoBuffer>,
    bounds: (u64, u64),
    /// The topology and point count the records were built from.
    built: Option<(Topology, u32)>,
}

impl Triangulator {
    pub fn new(device: &wgpu::Device) -> Self {
        use Bind::*;
        let src = |body: &str| {
            // `span` reads `start`, declared by each body.
            format!("{}{COMMON}{body}", header::WGSL)
        };
        let headers = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: header::BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
                mapped_at_creation: false,
            })
        };
        Self {
            count: Kernel::new(
                device,
                "img.render-count",
                &src(COUNT),
                "count",
                &[Uniform, Read, Read, ReadWrite, ReadWrite],
            ),
            finish: Kernel::new(
                device,
                "img.render-finish",
                &format!("{}{}{FINISH}", header::WGSL, COMMON_NO_SPAN),
                "finish",
                &[Uniform, Read, Read, ReadWrite, ReadWrite],
            ),
            emit_tri: Kernel::new(
                device,
                "img.render-emit-tri",
                &src(EMIT),
                "emit_tri",
                &[Uniform, Read, Read, Read, Read, ReadWrite],
            ),
            emit_seg: Kernel::new(
                device,
                "img.render-emit-seg",
                &src(EMIT),
                "emit_seg",
                &[Uniform, Read, Read, Read, Read, ReadWrite],
            ),
            params: Uniforms::new(device, "img.render-triangulate", 32),
            lists: None,
            tri_header: headers("img.render-tri-header"),
            seg_header: headers("img.render-seg-header"),
            tris: None,
            segs: None,
            bounds: (0, 0),
            built: None,
        }
    }

    /// Brings the records up to date with `geo`'s primitives, recording
    /// the passes into `encoder` when its topology or point count changed.
    /// After an error, or for a geometry without primitives, [`Self::ready`]
    /// is `None`.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pool: &BufferPool,
        encoder: &mut wgpu::CommandEncoder,
        geo: &Geometry,
    ) -> Result<(), String> {
        let Some(topo) = geo.topology().filter(|t| !t.is_empty()) else {
            self.built = None;
            return Ok(());
        };
        let points = geo.points();
        let same = self.built.as_ref().is_some_and(|(t, n)| {
            *n == points
                && t.kinds.shares(&topo.kinds)
                && t.start.shares(&topo.start)
                && t.corners.shares(&topo.corners)
        });
        if !same {
            self.build(device, queue, pool, encoder, topo, points)?;
        }
        Ok(())
    }

    /// The records of the last [`Self::update`], if it built any.
    pub fn ready(&self) -> Option<Records<'_>> {
        self.built.as_ref()?;
        Some(Records {
            tris: self.tris.as_ref()?,
            tri_header: &self.tri_header,
            segs: self.segs.as_ref()?,
            seg_header: &self.seg_header,
            any_tris: self.bounds.0 > 0,
            any_segs: self.bounds.1 > 0,
        })
    }

    fn build(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pool: &BufferPool,
        encoder: &mut wgpu::CommandEncoder,
        topo: &Topology,
        points: u32,
    ) -> Result<(), String> {
        self.built = None;
        let (tri_cap, seg_cap) = validate(topo, &device.limits())?;
        let prims = topo.len();
        // Scans are sized in powers of two, so a growing mesh does not
        // rebuild them on every change.
        let scan_n = prims.max(1).next_power_of_two();
        if self.lists.as_ref().is_none_or(|l| l.scan_n < scan_n) {
            let list = || pool.attr(u64::from(scan_n) * 4);
            self.lists = Some(Lists {
                scan_n,
                tri_scan: Scan::new(device, scan_n),
                seg_scan: Scan::new(device, scan_n),
                tri_n: list(),
                seg_n: list(),
                tri_off: list(),
                seg_off: list(),
            });
        }
        let records = |cap: u64| pool.attr(cap.max(1) * RECORD);
        if self
            .tris
            .as_ref()
            .is_none_or(|b| b.size() < tri_cap * RECORD)
        {
            self.tris = Some(records(tri_cap));
        }
        if self
            .segs
            .as_ref()
            .is_none_or(|b| b.size() < seg_cap * RECORD)
        {
            self.segs = Some(records(seg_cap));
        }
        self.bounds = (tri_cap, seg_cap);
        let lists = self.lists.as_ref().expect("made above");
        let kinds = topo.kinds.ensure_gpu(queue, pool);
        let start = topo.start.ensure_gpu(queue, pool);
        let corners = topo.corners.ensure_gpu(queue, pool);
        let mut tp = [0u8; 32];
        tp[..16].copy_from_slice(&words([prims, topo.corners.len(), points, lists.scan_n]));
        tp[16..].copy_from_slice(&words([tri_cap as u32, seg_cap as u32, 0, 0]));
        self.params.write(queue, &tp);
        let p = self.params.buffer();
        let tris = self.tris.as_ref().expect("made above");
        let segs = self.segs.as_ref().expect("made above");
        let mut cp = pass(encoder);
        self.count.dispatch_in(
            device,
            &mut cp,
            &[p, kinds, start, &lists.tri_n, &lists.seg_n],
            Work::Elements(lists.scan_n),
        );
        let tri_total = lists
            .tri_scan
            .record(device, &mut cp, &lists.tri_n, &lists.tri_off);
        let seg_total = lists
            .seg_scan
            .record(device, &mut cp, &lists.seg_n, &lists.seg_off);
        self.finish.dispatch_in(
            device,
            &mut cp,
            &[p, tri_total, seg_total, &self.tri_header, &self.seg_header],
            Work::Groups(1),
        );
        self.emit_tri.dispatch_in(
            device,
            &mut cp,
            &[p, &self.tri_header, &lists.tri_off, start, corners, tris],
            Work::Header(&self.tri_header),
        );
        self.emit_seg.dispatch_in(
            device,
            &mut cp,
            &[p, &self.seg_header, &lists.seg_off, start, corners, segs],
            Work::Header(&self.seg_header),
        );
        drop(cp);
        self.built = Some((topo.clone(), points));
        Ok(())
    }
}

/// The parameter struct without `span` (the finish pass reads no topology).
const COMMON_NO_SPAN: &str = r#"
struct TP { prims: u32, corners: u32, points: u32, scan_n: u32, tri_cap: u32, seg_cap: u32, pad0: u32, pad1: u32 };
@group(0) @binding(0) var<uniform> tp: TP;
"#;
