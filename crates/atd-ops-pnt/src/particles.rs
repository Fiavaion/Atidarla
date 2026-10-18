//! `pnt.particles`: a GPU particle system (PT-2; design §4).
//!
//! The state is the GEO value itself (design D6): the previous frame's
//! particles come from the `prev` feedback input when it is wired (the end
//! of a chain of force operators), else from this node's own last result.
//! Each frame, on the GPU, with a fixed capacity so a running simulation
//! never allocates (D5):
//!
//! 1. **flag** survivors: `age + dt < life`;
//! 2. **compact** (D7): an integer prefix sum of the flags gives each
//!    survivor its new index, in order, so the result does not depend on
//!    thread timing; the header gets `count = survivors + emitted` (at most
//!    the capacity);
//! 3. **gather** `P`, `v`, `age`, `life`, `id` into the new generation;
//! 4. **emit** new particles after the survivors, each from a hash of
//!    (seed, emission number): no RNG state, no atomics;
//! 5. **integrate**: forces (curl noise, attraction to a point, gravity,
//!    drag), then `v += a·dt`, `P += v·dt`, `age += dt`, and `Cd` from age.
//!
//! The count never leaves the GPU: later passes dispatch, and Render
//! Points draws, from the header (`dispatch_workgroups_indirect`,
//! `draw_indirect`).

use std::sync::Arc;

use atd_core::geo::{AttrType, header, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{
    Bind, Compactor, Gather, GatherMap, Kernel, Packer, Uniforms, Work, pass,
};
use atd_gpu::geo::{Attribute, Geometry, wgsl};

/// Most particles (a 1-D dispatch reaches 65,535 × 256 elements).
pub const MAX_CAPACITY: i64 = 16_000_000;

const EMITTERS: &[&str] = &["point", "sphere", "box"];

pub fn schema() -> OperatorSchema {
    let mut s = crate::pnt(
        "particles",
        "Particles",
        "A GPU particle system: emits particles at a rate, ages them, removes them at the end of their life and moves them with curl noise, attraction to a point, gravity and drag. Writes `P`, `v`, `age`, `life`, `id` and `Cd`; the live count stays on the GPU. Wire the end of a chain of force operators into `prev` to carry their effect to the next frame.",
        &[("prev", true)],
        vec![
            ParamSchema::int("capacity", "Capacity", 1_000_000, Some(1), Some(MAX_CAPACITY)).help(
                "Most particles alive at once; memory is set aside for this many. Changing it restarts the system.",
            ),
            ParamSchema::float("rate", "Rate", 100_000.0, Some(0.0), None)
                .help("Particles emitted per second (none once the capacity is reached)."),
            ParamSchema::int("initial", "Initial", 0, Some(0), Some(MAX_CAPACITY))
                .help("Particles emitted on the first frame (and on reset), with ages spread over their life."),
            ParamSchema::float("life", "Life", 4.0, Some(0.001), None)
                .help("Seconds a particle lives."),
            ParamSchema::float("life_variance", "Life Variance", 0.25, Some(0.0), Some(1.0))
                .help("Random variation of the life, as a fraction (0.25: ±25%)."),
            ParamSchema::menu("emitter", "Emitter", EMITTERS, "sphere")
                .help("Where particles are born: at the centre, in a ball or in a box."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("Centre of the emitter."),
            ParamSchema::float("radius", "Radius", 0.25, Some(0.0), None)
                .help("Radius of the ball, or half the side of the box."),
            ParamSchema::vec("velocity", "Velocity", &[0.0, 0.0, 0.0])
                .help("Initial velocity (units per second)."),
            ParamSchema::float("spread", "Spread", 0.3, Some(0.0), None)
                .help("Random initial speed added in any direction, up to this."),
            ParamSchema::float("curl_amplitude", "Curl Amplitude", 1.0, None, None)
                .help("Strength of the curl-noise force (units per second²)."),
            ParamSchema::float("curl_frequency", "Curl Frequency", 1.5, Some(0.0), None)
                .help("Noise cells per unit of the curl-noise field."),
            ParamSchema::float("curl_speed", "Curl Speed", 0.2, None, None)
                .help("How fast the curl-noise field evolves (field units per second)."),
            ParamSchema::vec("attract_point", "Attract Point", &[0.0, 0.0, 0.0])
                .help("Point particles are pulled toward."),
            ParamSchema::float("attract", "Attract", 0.5, None, None)
                .help("Pull toward the point per unit of distance (a spring; negative pushes away)."),
            ParamSchema::vec("gravity", "Gravity", &[0.0, 0.0, 0.0])
                .help("Constant acceleration (units per second²)."),
            ParamSchema::float("drag", "Drag", 0.5, Some(0.0), None)
                .help("Velocity lost per second, as a fraction."),
            ParamSchema::colour("colour_birth", "Birth Colour", [1.0, 0.8, 0.4, 1.0])
                .help("`Cd` of a new particle."),
            ParamSchema::colour("colour_death", "Death Colour", [0.2, 0.3, 1.0, 0.0])
                .help("`Cd` at the end of its life (in between, mixed by age)."),
            ParamSchema::int("seed", "Seed", 1, Some(0), None)
                .help("Random seed; the same seed and frames give the same particles."),
            ParamSchema::toggle("reset", "Reset", false)
                .help("While on, restarts the system every frame (empty, then the initial burst)."),
        ],
        300,
    );
    s.time_dependent = true;
    s
}

const FLAGS: &str = r#"
struct Params { dt: f32, pad0: u32, pad1: u32, pad2: u32 };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@group(0) @binding(2) var<storage, read> age: array<f32>;
@group(0) @binding(3) var<storage, read> life: array<f32>;
@group(0) @binding(4) var<storage, read_write> flags: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.x;
    if (i >= hdr.capacity) { return; }
    var keep = 0u;
    if (i < hdr.count && age[i] + params.dt < life[i]) { keep = 1u; }
    flags[i] = keep;
}
"#;

const EMIT: &str = r#"
struct Params {
    // emitter centre; w: radius
    centre: vec4f,
    // initial velocity; w: spread
    velocity: vec4f,
    // life, variance
    life: vec4f,
    base: u32,
    seed: u32,
    emitter: u32,
    // The first `prewarm` emissions of this frame get ages spread over
    // their life (the initial burst).
    prewarm: u32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> v: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> age: array<f32>;
@group(0) @binding(5) var<storage, read_write> life: array<f32>;
@group(0) @binding(6) var<storage, read_write> id: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let j = gid.x;
    if (j < hdr.mapped || j >= hdr.count) { return; }
    // The emission number: the same particle on every run.
    let k = j - hdr.mapped;
    let e = params.base + k;
    var p = params.centre.xyz;
    if (params.emitter == 1u) {
        p = p + atd_in_sphere(params.seed ^ 0x2545f491u, e) * params.centre.w;
    } else if (params.emitter == 2u) {
        p = p + (atd_rand3(params.seed ^ 0x68e31da4u, e) * 2.0 - vec3f(1.0)) * params.centre.w;
    }
    let vel = params.velocity.xyz + atd_in_sphere(params.seed ^ 0x9e3779b9u, e) * params.velocity.w;
    let r = atd_rand(params.seed ^ 0x85ebca6bu, e) * 2.0 - 1.0;
    let l = max(params.life.x * (1.0 + params.life.y * r), 0.001);
    var a = 0.0;
    if (k < params.prewarm) { a = atd_rand(params.seed ^ 0xc2b2ae35u, e) * l; }
    P[j] = vec4f(p, 0.0);
    v[j] = vec4f(vel, 0.0);
    age[j] = a;
    life[j] = l;
    id[j] = e;
}
"#;

const INTEGRATE: &str = r#"
struct Params {
    // attraction point; w: strength
    attract: vec4f,
    // gravity; w: drag
    gravity: vec4f,
    // curl amplitude, frequency, field offset (time × speed), dt
    curl: vec4f,
    birth: vec4f,
    death: vec4f,
    seed: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> v: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> age: array<f32>;
@group(0) @binding(5) var<storage, read> life: array<f32>;
@group(0) @binding(6) var<storage, read_write> Cd: array<vec4f>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.x;
    if (i >= hdr.count) { return; }
    let dt = params.curl.w;
    var p = P[i].xyz;
    var vel = v[i].xyz;
    let q = p * params.curl.y + vec3f(0.0, 0.0, params.curl.z);
    var acc = atd_curl(q, params.seed) * params.curl.x;
    acc = acc + (params.attract.xyz - p) * params.attract.w;
    acc = acc + params.gravity.xyz - vel * params.gravity.w;
    vel = vel + acc * dt;
    p = p + vel * dt;
    let a = age[i] + dt;
    P[i] = vec4f(p, 0.0);
    v[i] = vec4f(vel, 0.0);
    age[i] = a;
    Cd[i] = mix(params.birth, params.death, clamp(a / life[i], 0.0, 1.0));
}
"#;

/// Buffers the compaction needs, sized to the capacity.
struct Sized {
    capacity: u32,
    compactor: Compactor,
    flags: wgpu::Buffer,
}

pub struct Particles {
    flag: Kernel,
    emit: Kernel,
    integrate: Kernel,
    gather: Gather,
    flag_params: Uniforms,
    emit_params: Uniforms,
    integrate_params: Uniforms,
    sized: Option<Sized>,
    /// The frame's header as the kernels write and bind it (a fixed buffer,
    /// so their bind groups stay cached however the pool hands out headers);
    /// copied into the pooled output header at the end.
    work: wgpu::Buffer,
    /// Fractional emissions carried to the next frame.
    carry: f64,
    /// Emission number of the next particle (wraps; ids are unique among
    /// live particles while the capacity is below 2³²).
    emitted: u32,
    /// The next cook starts the system (first cook, reset, capacity).
    restart: bool,
    /// Attribute names, made once (`CARRIED` order, then `Cd`), so a frame
    /// does not allocate them.
    names: [Arc<str>; 6],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    let lib = format!("{}{}{}", wgsl::HASH, wgsl::NOISE, header::WGSL);
    Box::new(Particles {
        flag: Kernel::new(
            device,
            "pnt.particles.flag",
            &format!("{lib}{FLAGS}"),
            "main",
            &[Uniform, Read, Read, Read, ReadWrite],
        ),
        emit: Kernel::new(
            device,
            "pnt.particles.emit",
            &format!("{lib}{EMIT}"),
            "main",
            &[
                Uniform, Read, ReadWrite, ReadWrite, ReadWrite, ReadWrite, ReadWrite,
            ],
        ),
        integrate: Kernel::new(
            device,
            "pnt.particles.integrate",
            &format!("{lib}{INTEGRATE}"),
            "main",
            &[
                Uniform, Read, ReadWrite, ReadWrite, ReadWrite, Read, ReadWrite,
            ],
        ),
        gather: Gather::new(device),
        flag_params: Uniforms::new(device, "pnt.particles.flag", 16),
        emit_params: Uniforms::new(device, "pnt.particles.emit", 64),
        integrate_params: Uniforms::new(device, "pnt.particles.integrate", 96),
        sized: None,
        work: crate::kit::work_header(device),
        carry: 0.0,
        emitted: 0,
        restart: true,
        names: [
            names::P,
            names::V,
            names::AGE,
            names::LIFE,
            names::ID,
            names::CD,
        ]
        .map(Arc::from),
    })
}

/// The attributes the state carries from frame to frame, in gather order.
const CARRIED: [(&str, AttrType); 5] = [
    (names::P, AttrType::VEC3),
    (names::V, AttrType::VEC3),
    (names::AGE, AttrType::F32),
    (names::LIFE, AttrType::F32),
    (names::ID, AttrType::U32),
];

/// The previous state's header and carried buffers, if `g` is a particle
/// state of `capacity`.
fn state(g: &Geometry, capacity: u32) -> Option<(wgpu::Buffer, [wgpu::Buffer; 5])> {
    if !g.is_gpu_counted() || g.points() != capacity {
        return None;
    }
    let header = g.header()?.clone();
    let mut out: [Option<wgpu::Buffer>; 5] = Default::default();
    for (slot, (name, ty)) in out.iter_mut().zip(CARRIED) {
        *slot = Some(g.point_typed(name, ty)?.gpu_buffer()?.clone());
    }
    Some((header, out.map(|b| b.expect("filled above"))))
}

impl GeoOp for Particles {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let capacity = p.i64("capacity").clamp(1, MAX_CAPACITY) as u32;
        let max = ctx.device.limits().max_storage_buffer_binding_size / 16;
        if u64::from(capacity) > max {
            ctx.output.clear();
            return Err(format!(
                "a capacity of {capacity} needs more than this GPU's {} MiB storage binding; use at most {max}",
                ctx.device.limits().max_storage_buffer_binding_size >> 20
            ));
        }
        if self.sized.as_ref().is_none_or(|s| s.capacity != capacity) {
            // An edit, not a frame-critical path: buffers sized once here.
            self.sized = Some(Sized {
                capacity,
                compactor: Compactor::new(ctx.device, capacity),
                flags: ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pnt.particles.flags"),
                    size: atd_core::geo::align_up(u64::from(capacity) * 4),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            });
            self.restart = true;
        }
        if p.bool("reset") {
            self.restart = true;
        }
        let restart = std::mem::take(&mut self.restart);
        let dt = ctx.dt as f32;

        // The previous state: the feedback input when wired, else our own.
        let prev = if restart {
            None
        } else if ctx.input(0).is_some() {
            ctx.input(0).and_then(|g| state(g, capacity))
        } else {
            state(ctx.output, capacity)
        };

        // How many to emit this frame (the GPU clamps to the capacity): on
        // a restart the initial burst first, then the rate's share.
        let mut append = 0u32;
        if restart {
            self.carry = 0.0;
            self.emitted = 0;
            append = p.i64("initial").clamp(0, MAX_CAPACITY) as u32;
        }
        let prewarm = append;
        self.carry += f64::from(p.f32("rate").max(0.0)) * ctx.dt;
        let whole = self.carry.floor();
        self.carry -= whole;
        append = append.saturating_add(whole.min(f64::from(u32::MAX)) as u32);

        let sized = self.sized.as_ref().expect("sized above");
        let flags = &sized.flags;
        let new_header = Arc::new(ctx.buffers.attr(header::BYTES));
        let hdr = self.work.clone();

        // The new generation, from the pool (cleared offline).
        let mut attrs: [Option<Attribute>; 5] = Default::default();
        for ((slot, (_, ty)), name) in attrs.iter_mut().zip(CARRIED).zip(&self.names) {
            *slot = Some(ctx.alloc(name.clone(), ty, capacity));
        }
        let attrs = attrs.map(|a| a.expect("allocated above"));
        let cd = ctx.alloc(self.names[5].clone(), AttrType::VEC4, capacity);
        let bufs = attrs
            .each_ref()
            .map(|a| a.gpu_buffer().expect("on the GPU").clone());
        let cd_buf = cd.gpu_buffer().expect("on the GPU").clone();
        if prev.is_none() {
            ctx.encoder.clear_buffer(flags, 0, None);
        }

        // Parameters of the passes (written before submit).
        let mut u = Packer::<16>::new();
        u.f32(dt);
        self.flag_params.write(ctx.queue, u.bytes());
        let emitter = EMITTERS
            .iter()
            .position(|e| *e == p.str("emitter"))
            .unwrap_or(1) as u32;
        let centre = p.vec4("centre");
        let velocity = p.vec4("velocity");
        let seed = p.i64("seed").max(0) as u32;
        let mut u = Packer::<64>::new();
        u.vec4([centre[0], centre[1], centre[2], p.f32("radius").max(0.0)])
            .vec4([
                velocity[0],
                velocity[1],
                velocity[2],
                p.f32("spread").max(0.0),
            ])
            .vec4([
                p.f32("life").max(0.001),
                p.f32("life_variance").clamp(0.0, 1.0),
                0.0,
                0.0,
            ])
            .u32(self.emitted)
            .u32(seed)
            .u32(emitter)
            .u32(prewarm);
        self.emit_params.write(ctx.queue, u.bytes());
        self.emitted = self.emitted.wrapping_add(append);
        let attract = p.vec4("attract_point");
        let gravity = p.vec4("gravity");
        let mut u = Packer::<96>::new();
        u.vec4([attract[0], attract[1], attract[2], p.f32("attract")])
            .vec4([gravity[0], gravity[1], gravity[2], p.f32("drag").max(0.0)])
            .vec4([
                p.f32("curl_amplitude"),
                p.f32("curl_frequency").max(0.0),
                (ctx.time * f64::from(p.f32("curl_speed"))) as f32,
                dt,
            ])
            .vec4(p.vec4("colour_birth"))
            .vec4(p.vec4("colour_death"))
            .u32(seed);
        self.integrate_params.write(ctx.queue, u.bytes());

        // Every step in one compute pass (wgpu allocates per pass and
        // orders the dispatches of a pass).
        let device = ctx.device;
        let mut pass = pass(ctx.encoder);
        // 1-2. Flag survivors and compact.
        if let Some((prev_hdr, [_, _, age, life, _])) = &prev {
            self.flag.dispatch_in(
                device,
                &mut pass,
                &[self.flag_params.buffer(), prev_hdr, age, life, flags],
                Work::Elements(capacity),
            );
        }
        sized
            .compactor
            .record(device, ctx.queue, &mut pass, flags, append, &hdr);
        // 3. Gather survivors into the new generation.
        if let Some((_, src)) = &prev {
            let map = GatherMap {
                header: &hdr,
                map: sized.compactor.map(),
            };
            for ((src, dst), (_, ty)) in src.iter().zip(&bufs).zip(CARRIED) {
                self.gather
                    .record(device, &mut pass, map, ty.stride(), src, dst);
            }
        }
        // 4. Emit after the survivors.
        let [pos, vel, age, life, id] = &bufs;
        self.emit.dispatch_in(
            device,
            &mut pass,
            &[self.emit_params.buffer(), &hdr, pos, vel, age, life, id],
            Work::Header(&hdr),
        );
        // 5. Forces and integration; colour from age.
        self.integrate.dispatch_in(
            device,
            &mut pass,
            &[
                self.integrate_params.buffer(),
                &hdr,
                pos,
                vel,
                age,
                life,
                &cd_buf,
            ],
            Work::Header(&hdr),
        );
        drop(pass);
        ctx.encoder
            .copy_buffer_to_buffer(&hdr, 0, new_header.buffer(), 0, header::BYTES);

        // The new generation replaces the old, whose buffers return to the
        // pool once this frame is submitted.
        let out = &mut *ctx.output;
        out.clear();
        out.set_gpu_counted(capacity, new_header);
        for a in attrs {
            out.set_point(a);
        }
        out.set_point(cd);
        Ok(())
    }
}
