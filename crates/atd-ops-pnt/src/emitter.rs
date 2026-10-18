//! `pnt.emitter`: emits new particles from the points of a `source`
//! geometry into a particle state (design §4.3, as `pnt.particles` emits).
//!
//! The state on `in` keeps its capacity: new particles are appended after
//! its live ones, `count = min(count + n, capacity)`, written by a
//! one-thread header kernel so the count never leaves the GPU. Each new
//! particle is a hash of (seed, emission number): it starts at a random
//! source point (with its velocity and normal), so runs repeat exactly.
//! Typical use: Particles (rate 0, or its own emitter) → Emitter → `prev`.

use std::sync::Arc;

use atd_core::geo::{AttrType, header, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{Bind, Kernel, Packer, Uniforms, Work, pass};

use crate::kit::{self, Missing, PointKernel, vec3};

pub fn schema() -> OperatorSchema {
    let mut s = crate::pnt(
        "emitter",
        "Emitter",
        "Emits particles from the points of `source` into the particle state on `in` (for example a Particles system with rate 0, in its feedback loop): Rate per second from random source points, with a velocity, a speed along the source's normal `N`, a share of its velocity `v` and a random spread. Writes `P`, `v`, `age`, `life`, `id` (and `Cd` when the state has it) of the new particles; the state's capacity limits the count.",
        &[("in", false), ("source", false)],
        vec![
            ParamSchema::float("rate", "Rate", 10_000.0, Some(0.0), None).help("Particles emitted per second."),
            ParamSchema::float("life", "Life", 3.0, Some(0.001), None).help("Seconds a new particle lives."),
            ParamSchema::float("life_variance", "Life Variance", 0.25, Some(0.0), Some(1.0)).help("Random variation of the life, as a fraction."),
            ParamSchema::vec("velocity", "Velocity", &[0.0, 0.0, 0.0]).help("Initial velocity added to every particle."),
            ParamSchema::float("normal_speed", "Normal Speed", 1.0, None, None).help("Speed along the source point's `N` (when it has one)."),
            ParamSchema::float("inherit", "Inherit", 0.0, None, None).help("Share of the source point's `v` the particle starts with."),
            ParamSchema::float("spread", "Spread", 0.2, Some(0.0), None).help("Random speed added in any direction, up to this."),
            ParamSchema::float("jitter", "Jitter", 0.0, Some(0.0), None).help("Random offset from the source point, up to this distance."),
            ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0]).help("`Cd` of a new particle, when the state has `Cd`."),
            ParamSchema::int("id_offset", "ID Offset", 1 << 30, Some(0), None).help("Added to the emission number to make `id`, so these particles' ids differ from another emitter's."),
            ParamSchema::int("seed", "Seed", 1, Some(0), None).help("Random seed."),
            ParamSchema::toggle("reset", "Reset", false).help("While on, the emission count restarts (ids and random choices start over)."),
        ],
        150,
    );
    s.time_dependent = true;
    s
}

const HEADER: &str = "struct Params { append: vec4u };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> old: GeoHeader;
@group(0) @binding(2) var<storage, read_write> hdr: GeoHeader;
@compute @workgroup_size(1)
fn main() {
    let cap = params.append.y;
    let live = min(old.count, cap);
    let count = live + min(params.append.x, cap - live);
    hdr.count = count;
    hdr.capacity = cap;
    hdr.mapped = live;
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

const COMMON: &str = "struct Params {
    velocity: vec4f,
    // normal speed, inherit, spread, jitter
    speeds: vec4f,
    // life, variance, the frame's step, unused
    life: vec4f,
    colour: vec4f,
    base: u32,
    seed: u32,
    id_offset: u32,
    // bit 0: source N; 1: source v; 2: state Cd
    flags: u32,
};
fn source_index(e: u32) -> u32 {
    return atd_hash(e ^ atd_hash(params.seed ^ 0x7f4a7c15u)) % max(hs.count, 1u);
}
";

const MOVE: &str = "@group(0) @binding(2) var<storage, read> hs: GeoHeader;
@group(0) @binding(3) var<storage, read> SP: array<vec4f>;
@group(0) @binding(4) var<storage, read> SN: array<vec4f>;
@group(0) @binding(5) var<storage, read> SV: array<vec4f>;
@group(0) @binding(6) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(7) var<storage, read_write> v: array<vec4f>;
fn point(j: u32) {
    if (j < hdr.mapped) { return; }
    let e = params.base + (j - hdr.mapped);
    let s = source_index(e);
    var vel = params.velocity.xyz + atd_in_sphere(params.seed ^ 0x9e3779b9u, e) * params.speeds.z;
    if ((params.flags & 1u) != 0u) { vel = vel + SN[s].xyz * params.speeds.x; }
    if ((params.flags & 2u) != 0u) { vel = vel + SV[s].xyz * params.speeds.y; }
    // Births are spread over the frame's step (no bands of particles born together).
    let early = atd_rand(params.seed ^ 0x1b873593u, e) * params.life.z;
    let p = SP[s].xyz + atd_in_sphere(params.seed ^ 0x2545f491u, e) * params.speeds.w + vel * early;
    P[j] = vec4f(p, 0.0);
    v[j] = vec4f(vel, 0.0);
}
";

const LIFE: &str = "@group(0) @binding(2) var<storage, read> hs: GeoHeader;
@group(0) @binding(3) var<storage, read_write> age: array<f32>;
@group(0) @binding(4) var<storage, read_write> life: array<f32>;
@group(0) @binding(5) var<storage, read_write> ID: array<u32>;
@group(0) @binding(6) var<storage, read_write> Cd: array<vec4f>;
fn point(j: u32) {
    if (j < hdr.mapped) { return; }
    let e = params.base + (j - hdr.mapped);
    let r = atd_rand(params.seed ^ 0x85ebca6bu, e) * 2.0 - 1.0;
    age[j] = 0.0;
    life[j] = max(params.life.x * (1.0 + params.life.y * r), 0.001);
    ID[j] = params.id_offset + e;
    if ((params.flags & 4u) != 0u) { Cd[j] = params.colour; }
}
";

pub struct Emitter {
    header: Kernel,
    append: Uniforms,
    motion: PointKernel,
    life: PointKernel,
    dummy: wgpu::Buffer,
    /// Bound by the kernels; copied into the pooled output header.
    work: wgpu::Buffer,
    carry: f64,
    emitted: u32,
    started: bool,
    names: [Arc<str>; 5],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Emitter {
        header: Kernel::new(
            device,
            "pnt.emitter.header",
            &format!("{}{HEADER}", header::WGSL),
            "main",
            &[Uniform, Read, ReadWrite],
        ),
        append: Uniforms::new(device, "pnt.emitter.append", 16),
        motion: PointKernel::new(
            device,
            "pnt.emitter.motion",
            &format!("{COMMON}{MOVE}"),
            &[Read, Read, Read, Read, ReadWrite, ReadWrite],
            80,
        ),
        life: PointKernel::new(
            device,
            "pnt.emitter.life",
            &format!("{COMMON}{LIFE}"),
            &[Read, ReadWrite, ReadWrite, ReadWrite, ReadWrite],
            80,
        ),
        dummy: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.emitter.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
        work: kit::work_header(device),
        carry: 0.0,
        emitted: 0,
        started: false,
        names: [names::P, names::V, names::AGE, names::LIFE, names::ID].map(Arc::from),
    })
}

impl GeoOp for Emitter {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        let p = ctx.params;
        if !self.started || p.bool("reset") {
            self.started = true;
            self.carry = 0.0;
            self.emitted = 0;
        }
        self.carry += f64::from(p.f32("rate").max(0.0)) * ctx.dt;
        let whole = self.carry.floor();
        self.carry -= whole;
        let append = whole.min(f64::from(u32::MAX)) as u32;
        let source = ctx.input(1).filter(|s| s.points() > 0).cloned();
        let (Some(source), true) = (source, g.points() > 0) else {
            *ctx.output = g;
            return Ok(());
        };
        let capacity = g.points();
        let old = kit::header_of(&g)?;
        let hs = kit::header_of(&source)?;
        let sp = kit::readable(ctx, &source, names::P, AttrType::VEC3)?;
        let sn = source
            .point_typed(names::N, AttrType::VEC3)
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        let sv = source
            .point_typed(names::V, AttrType::VEC3)
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        // The state's attributes, writable (created when missing).
        let attr = |ctx: &mut GeoCtx<'_>, g: &mut atd_gpu::geo::Geometry, k: usize, ty| {
            let name = self.names[k].clone();
            kit::writable(ctx, g, &name, ty, Missing::Zero)
        };
        let pos = attr(ctx, &mut g, 0, AttrType::VEC3)?;
        let vel = attr(ctx, &mut g, 1, AttrType::VEC3)?;
        let age = attr(ctx, &mut g, 2, AttrType::F32)?;
        let life = attr(ctx, &mut g, 3, AttrType::F32)?;
        let id = attr(ctx, &mut g, 4, AttrType::U32)?;
        let cd = if g.point_typed(names::CD, AttrType::VEC4).is_some() {
            Some(kit::writable(
                ctx,
                &mut g,
                names::CD,
                AttrType::VEC4,
                Missing::Error,
            )?)
        } else {
            None
        };
        let header = kit::new_header(ctx);
        let hdr = self.work.clone();
        self.append.write(
            ctx.queue,
            &atd_gpu::geo::kernels::words([append, capacity, 0, 0]),
        );
        let mut flags = 0u32;
        if sn.is_some() {
            flags |= 1;
        }
        if sv.is_some() {
            flags |= 2;
        }
        if cd.is_some() {
            flags |= 4;
        }
        let mut u = Packer::<80>::new();
        u.vec4(vec3(&p, "velocity"))
            .vec4([
                p.f32("normal_speed"),
                p.f32("inherit"),
                p.f32("spread").max(0.0),
                p.f32("jitter").max(0.0),
            ])
            .vec4([
                p.f32("life").max(0.001),
                p.f32("life_variance").clamp(0.0, 1.0),
                ctx.dt as f32,
                0.0,
            ])
            .vec4(p.vec4("colour"))
            .u32(self.emitted)
            .u32(kit::seed(&p))
            .u32(p.i64("id_offset").clamp(0, i64::from(u32::MAX)) as u32)
            .u32(flags);
        self.motion.write(ctx.queue, u.bytes());
        self.life.write(ctx.queue, u.bytes());
        self.emitted = self.emitted.wrapping_add(append);
        let d = &self.dummy;
        let device = ctx.device;
        let mut ps = pass(ctx.encoder);
        self.header.dispatch_in(
            device,
            &mut ps,
            &[self.append.buffer(), &old, &hdr],
            Work::Groups(1),
        );
        self.motion.run_in(
            device,
            &mut ps,
            &hdr,
            &[
                &hs,
                &sp,
                sn.as_ref().unwrap_or(d),
                sv.as_ref().unwrap_or(d),
                &pos,
                &vel,
            ],
        );
        // Without `Cd` the placeholder is bound (written by no thread).
        let cd = cd.as_ref().unwrap_or(d);
        self.life
            .run_in(device, &mut ps, &hdr, &[&hs, &age, &life, &id, cd]);
        drop(ps);
        ctx.encoder
            .copy_buffer_to_buffer(&hdr, 0, header.buffer(), 0, header::BYTES);
        g.set_gpu_counted(capacity, header);
        *ctx.output = g;
        Ok(())
    }
}
