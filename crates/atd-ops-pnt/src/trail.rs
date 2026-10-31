//! Trail (history) and Flock (boids).
//!
//! **Trail** keeps the last Length positions of every point in a ring
//! buffer of its own (allocated when the point count or Length changes,
//! never per frame) and outputs them as one polyline per point, newest
//! first, with `age` 0 (now) to 1 (oldest). GPU-counted input (particles)
//! is followed slot by slot: a slot's trail stays where its particle died
//! until a new particle takes the slot.
//!
//! **Flock** steers the velocity `v` by Reynolds' three rules over the
//! neighbours within a radius (GPU grid hash): separation, alignment and
//! cohesion, then clamps the speed. It reads the previous velocities and
//! writes new ones (a pooled buffer), so every point sees the same frame
//! and the result repeats exactly; pair it with Integrate in a particle or
//! Solver loop.

use std::sync::Arc;

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp, Params};
use atd_gpu::geo::kernels::{Bind, Packer, Work, pass};
use atd_gpu::geo::{Geometry, PrimKind, Topology};

use crate::kit::{self, PointKernel};
use crate::neighbours::{Grid, QUERY, QUERY_BINDINGS};

// ---------------------------------------------------------------------- Trail

/// Most positions a trail keeps.
pub const MAX_LENGTH: i64 = 64;

pub fn trail_schema() -> OperatorSchema {
    let mut s = crate::pnt(
        "trail",
        "Trail",
        "The recent history of every point as a line: the last Length positions, newest first, one polyline per point (Render Points draws them with Draw set to lines). Writes `P`, `age` (0 now to 1 oldest), `Cd` (the point's colour, fading out with age when Fade is on) and `id` (the point's number). The history restarts when the point count or Length changes. Particles are followed slot by slot.",
        &[("in", false)],
        vec![
            ParamSchema::int("length", "Length", 16, Some(2), Some(MAX_LENGTH))
                .help("Positions kept per point (frames of history)."),
            ParamSchema::toggle("fade", "Fade", true)
                .help("Multiplies `Cd`'s alpha by 1 − age, so trails fade out."),
            ParamSchema::pulse("reset", "Reset")
                .help("Restarts every trail at its point's current position."),
        ],
        150,
    );
    s.time_dependent = true;
    s
}

const RECORD: &str = "struct Params {
    // points, length, head, reset
    t: vec4u,
};
@group(0) @binding(2) var<storage, read> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> H: array<vec4f>;
fn point(i: u32) {
    let l = params.t.y;
    let p = P[i];
    if (params.t.w == 1u) {
        for (var k = 0u; k < l; k = k + 1u) { H[i * l + k] = p; }
    } else {
        H[i * l + params.t.z] = p;
    }
}
";

const EMIT: &str = "struct Params {
    // points, length, head, fade
    t: vec4u,
    // 1: a colour is bound
    c: vec4u,
};
@group(0) @binding(2) var<storage, read> H: array<vec4f>;
@group(0) @binding(3) var<storage, read> CdIn: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(5) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(6) var<storage, read_write> AGE: array<f32>;
@group(0) @binding(7) var<storage, read_write> ID: array<u32>;
";

const EMIT_MAIN: &str = "let l = params.t.y;
    if (i < params.t.x * l) {
        let src = i / l;
        let k = i % l;
        let slot = (params.t.z + l - k) % l;
        P[i] = H[src * l + slot];
        let age = f32(k) / f32(l - 1u);
        var c = vec4f(1.0);
        if (params.c.x == 1u) { c = CdIn[src]; }
        if (params.t.w == 1u) { c.a = c.a * (1.0 - age); }
        Cd[i] = c;
        AGE[i] = age;
        ID[i] = src;
    }";

pub struct Trail {
    record: PointKernel,
    emit: PointKernel,
    /// The ring buffer: (points, length, buffer).
    history: Option<(u32, u32, wgpu::Buffer)>,
    /// One polyline per point, by (points, length).
    topology: Option<(u32, u32, Topology)>,
    head: u32,
    white: wgpu::Buffer,
    names: [Arc<str>; 4],
}

pub fn create_trail(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Trail {
        record: PointKernel::new(device, "pnt.trail.record", RECORD, &[Read, ReadWrite], 16),
        emit: PointKernel::with_main(
            device,
            "pnt.trail.emit",
            EMIT,
            &[Read, Read, ReadWrite, ReadWrite, ReadWrite, ReadWrite],
            32,
            EMIT_MAIN,
        ),
        history: None,
        topology: None,
        head: 0,
        white: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.trail.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
        names: [names::P, names::CD, names::AGE, names::ID].map(Arc::from),
    })
}

impl GeoOp for Trail {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(g) = ctx.input(0).filter(|g| g.points() > 0).cloned() else {
            ctx.output.clear();
            self.history = None;
            return Ok(());
        };
        let p = ctx.params;
        let n = g.points();
        let l = p.i64("length").clamp(2, MAX_LENGTH) as u32;
        let total = u64::from(n) * u64::from(l);
        let max = (ctx.device.limits().max_storage_buffer_binding_size / 16).min(16_000_000);
        if total > max {
            ctx.output.clear();
            return Err(format!(
                "{n} points × {l} positions is more than this GPU holds ({max})"
            ));
        }
        let total = total as u32;
        let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let cd = g
            .point_typed(names::CD, AttrType::VEC4)
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        let mut reset = p.bool("reset");
        if self.history.as_ref().is_none_or(|h| h.0 != n || h.1 != l) {
            let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pnt.trail.history"),
                size: u64::from(total) * 16,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.history = Some((n, l, buffer));
            self.head = 0;
            reset = true;
        } else {
            self.head = (self.head + 1) % l;
        }
        if self.topology.as_ref().is_none_or(|t| t.0 != n || t.1 != l) {
            let corners: Vec<u32> = (0..total).collect();
            let prims: Vec<(PrimKind, &[u32])> = corners
                .chunks_exact(l as usize)
                .map(|c| (PrimKind::Polyline, c))
                .collect();
            self.topology = Some((n, l, Topology::from_prims(&prims)));
        }
        let history = self.history.as_ref().expect("made above").2.clone();
        let hdr = kit::header_of(&g)?;
        let mut u = Packer::<16>::new();
        u.u32(n).u32(l).u32(self.head).u32(u32::from(reset));
        self.record.write(ctx.queue, u.bytes());
        let mut e = Packer::<32>::new();
        e.u32(n)
            .u32(l)
            .u32(self.head)
            .u32(u32::from(p.bool("fade")))
            .u32(u32::from(cd.is_some()));
        self.emit.write(ctx.queue, e.bytes());
        let out_hdr = ctx.cpu_header(total);
        let outs = [
            (AttrType::VEC3, 0),
            (AttrType::VEC4, 1),
            (AttrType::F32, 2),
            (AttrType::U32, 3),
        ]
        .map(|(ty, k)| ctx.alloc(self.names[k].clone(), ty, total));
        let b = [&outs[0], &outs[1], &outs[2], &outs[3]]
            .map(|a| a.gpu_buffer().expect("on the GPU").clone());
        let cd = cd.unwrap_or_else(|| self.white.clone());
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.record.run_in(device, &mut ps, &hdr, &[&pos, &history]);
        self.emit.dispatch_in(
            device,
            &mut ps,
            out_hdr.buffer(),
            &[&history, &cd, &b[0], &b[1], &b[2], &b[3]],
            Work::Elements(total),
        );
        drop(ps);
        let topology = self.topology.as_ref().expect("made above").2.clone();
        let out = &mut *ctx.output;
        out.clear();
        out.set_points(total, Some(out_hdr));
        for a in outs {
            out.set_point(a);
        }
        out.set_topology(Some(topology));
        Ok(())
    }
}

// ---------------------------------------------------------------------- Flock

pub fn flock_schema() -> OperatorSchema {
    crate::pnt(
        "flock",
        "Flock",
        "Boids: steers each point's velocity `v` (created at zero) by its neighbours within Radius: Separation pushes away from those closer than Personal Space, Alignment turns toward their average velocity, Cohesion toward their centre; the speed is then held between Min Speed and Max Speed. Forces act over the frame's step, like the other forces: put it with Integrate in a particle or Solver loop. Neighbours come from a GPU grid hash; every point reads the previous frame's velocities, so the result repeats exactly.",
        &[("in", false)],
        vec![
            ParamSchema::float("radius", "Radius", 0.3, Some(1e-6), None)
                .help("How far a point sees its neighbours (also the grid's cell size)."),
            ParamSchema::int("max_neighbours", "Max Neighbours", 32, Some(1), Some(256))
                .help("At most this many neighbours are used per point (in a fixed order)."),
            ParamSchema::float("separation", "Separation", 2.0, None, None)
                .help("Strength of the push away from neighbours closer than Personal Space."),
            ParamSchema::float("personal_space", "Personal Space", 0.1, Some(0.0), None)
                .help("Neighbours closer than this push the point away."),
            ParamSchema::float("alignment", "Alignment", 1.0, None, None)
                .help("Strength of the turn toward the neighbours' average velocity."),
            ParamSchema::float("cohesion", "Cohesion", 1.0, None, None)
                .help("Strength of the pull toward the neighbours' centre."),
            ParamSchema::float("min_speed", "Min Speed", 0.2, Some(0.0), None)
                .help("Slower points are sped up to this (along their heading, or +X when still)."),
            ParamSchema::float("max_speed", "Max Speed", 1.0, Some(0.0), None)
                .help("Faster points are slowed to this."),
        ],
        900,
    )
}

const FLOCK: &str = "struct Params {
    grid: vec4f,
    gi: vec4u,
    // separation, personal space, alignment, cohesion
    w: vec4f,
    // min speed, max speed, dt, unused
    s: vec4f,
};
@group(0) @binding(6) var<storage, read> V: array<vec4f>;
@group(0) @binding(7) var<storage, read_write> VN: array<vec4f>;
var<private> me: u32;
var<private> n: f32;
var<private> sep: vec3f;
var<private> vel: vec3f;
var<private> centre: vec3f;
fn visit(j: u32, d: vec3f, d2: f32) {
    if (j == me) { return; }
    n = n + 1.0;
    vel = vel + V[j].xyz;
    centre = centre + d;
    let ps = params.w.y;
    if (d2 < ps * ps && d2 > 1e-12) {
        sep = sep - d / d2 * (ps - sqrt(d2));
    }
}
fn point(i: u32) {
    me = i;
    n = 0.0;
    sep = vec3f(0.0);
    vel = vec3f(0.0);
    centre = vec3f(0.0);
    search(P[i].xyz);
    var v = V[i].xyz;
    if (n > 0.0) {
        let a = sep * params.w.x + (vel / n - v) * params.w.z + (centre / n) * params.w.w;
        v = v + a * params.s.z;
    }
    let speed = length(v);
    if (speed > params.s.y) { v = v / speed * params.s.y; }
    if (speed < params.s.x) {
        var dir = vec3f(1.0, 0.0, 0.0);
        if (speed > 1e-9) { dir = v / speed; }
        v = dir * params.s.x;
    }
    VN[i] = vec4f(v, V[i].w);
}
";

pub struct Flock {
    grid: Grid,
    kernel: PointKernel,
    zero: Option<(u32, wgpu::Buffer)>,
    names: [Arc<str>; 1],
}

pub fn create_flock(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Flock {
        grid: Grid::new(device),
        kernel: PointKernel::new(
            device,
            "pnt.flock",
            &format!("{FLOCK}{QUERY_BINDINGS}{QUERY}"),
            &[Read, Read, Read, Read, Read, ReadWrite],
            64,
        ),
        zero: None,
        names: [Arc::from(names::V)],
    })
}

fn flock_input(ctx: &mut GeoCtx<'_>) -> Option<Geometry> {
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

fn speeds(p: &Params<'_>) -> (f32, f32) {
    let min = p.f32("min_speed").max(0.0);
    (min, p.f32("max_speed").max(min))
}

impl GeoOp for Flock {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = flock_input(ctx) else {
            return Ok(());
        };
        let p = ctx.params;
        let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
        let n = g.points();
        let v = match g.point_typed(names::V, AttrType::VEC3) {
            Some(a) => a.ensure_gpu(ctx.queue, ctx.buffers).clone(),
            None => {
                // Still points: a zeroed buffer of our own (cleared once).
                if self.zero.as_ref().is_none_or(|z| z.0 < n) {
                    let b = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("pnt.flock.still"),
                        size: atd_core::geo::buffer_bytes(n, AttrType::VEC3),
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    ctx.encoder.clear_buffer(&b, 0, None);
                    self.zero = Some((n, b));
                }
                self.zero.as_ref().expect("made above").1.clone()
            }
        };
        let built = self.grid.build(
            ctx,
            &g,
            &pos,
            p.f32("radius"),
            p.i64("max_neighbours").clamp(1, 256) as u32,
        )?;
        let hdr = kit::header_of(&g)?;
        let out = ctx.alloc(self.names[0].clone(), AttrType::VEC3, n);
        let vn = out.gpu_buffer().expect("on the GPU").clone();
        let (min, max) = speeds(&p);
        let mut u = Packer::<64>::new();
        u.vec4(built.grid);
        for w in built.gi {
            u.u32(w);
        }
        u.vec4([
            p.f32("separation"),
            p.f32("personal_space").max(0.0),
            p.f32("alignment"),
            p.f32("cohesion"),
        ])
        .vec4([min, max, ctx.dt as f32, 0.0]);
        self.kernel.write(ctx.queue, u.bytes());
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.kernel.run_in(
            device,
            &mut ps,
            &hdr,
            &[&pos, &built.vals, &built.start, &built.end, &v, &vn],
        );
        drop(ps);
        g.set_point(out);
        *ctx.output = g;
        Ok(())
    }
}
