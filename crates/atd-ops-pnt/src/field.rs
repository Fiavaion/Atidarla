//! Per-point field operators, table-driven: forces on the velocity `v`
//! (Attract, Vortex, Drag, Gravity, Wind, Turbulence), operators on `P` and
//! `v` together (Collide, Integrate) and the deformers of `P` (Bend, Twist,
//! Taper, Wave).
//!
//! Each is one kernel over the live points that writes in place when the
//! engine hands the geometry over, else into a pooled copy of the attribute
//! it writes; every other attribute is shared. Forces add `a × dt` with the
//! frame's step, so in a particle loop (`pnt.particles` → forces → `prev`)
//! they act like the system's built-in forces; outside a loop they give a
//! one-step velocity.
//!
//! All share one uniform block: four `vec4f` (`a`-`d`), then `dt`, `seed`,
//! `mode` and `extra` (a `u32` such as an octave count).

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp, Params};
use atd_gpu::geo::kernels::{Bind, Packer};

use crate::kit::{self, Missing, PointKernel, menu_index, unit, vec3};

/// Which attributes a field operator binds (after the uniforms and header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// `v` read-write (created at zero when missing), `P` read.
    Velocity,
    /// `P` read-write.
    Position,
    /// `P` and `v` read-write (`v` created at zero when missing).
    Both,
}

/// The uniform values of one cook.
#[derive(Debug, Default, Clone, Copy)]
struct Packed {
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
    d: [f32; 4],
    seed: u32,
    mode: u32,
    extra: u32,
}

struct Spec {
    id: &'static str,
    label: &'static str,
    summary: &'static str,
    target: Target,
    params: fn() -> Vec<ParamSchema>,
    pack: fn(&Params<'_>) -> Packed,
    /// WGSL `fn point(i: u32)`.
    body: &'static str,
    budget_us: u32,
}

const DECL: &str = "struct Params {
    a: vec4f,
    b: vec4f,
    c: vec4f,
    d: vec4f,
    dt: f32,
    seed: u32,
    mode: u32,
    extra: u32,
};
";

const FALLOFF_HELP: &str =
    "Distance at which the effect fades out smoothly (0: no falloff, everywhere at full strength).";

const ATTRACT_MODES: &[&str] = &["spring", "constant", "inverse_square"];
const DRAG_MODES: &[&str] = &["linear", "quadratic"];
const COLLIDE_SHAPES: &[&str] = &["plane", "sphere_outside", "sphere_inside"];
const WAVE_MODES: &[&str] = &["linear", "radial"];

fn seed_param() -> ParamSchema {
    ParamSchema::int("seed", "Seed", 0, Some(0), None).help("Selects a different noise field.")
}

const SPECS: &[Spec] = &[
    Spec {
        id: "attract",
        label: "Attract",
        summary: "A force toward a point (or away from it with a negative strength) on the velocity `v` (created at zero when missing), reading `P`: a spring, a constant pull or an inverse-square pull, optionally fading out with distance.",
        target: Target::Velocity,
        params: || {
            vec![
                ParamSchema::vec("point", "Point", &[0.0, 0.0, 0.0]).help("The point particles are pulled toward."),
                ParamSchema::float("strength", "Strength", 1.0, None, None).help("Acceleration scale (negative repels)."),
                ParamSchema::menu("mode", "Mode", ATTRACT_MODES, "spring").help("spring: grows with distance; constant: the same at any distance; inverse_square: strongest close in (softened by Softening)."),
                ParamSchema::float("radius", "Radius", 0.0, Some(0.0), None).help(FALLOFF_HELP),
                ParamSchema::float("softening", "Softening", 0.01, Some(1e-6), None).help("inverse_square: the squared distance below which the force stops growing."),
            ]
        },
        pack: |p| {
            let c = vec3(p, "point");
            Packed {
                a: [c[0], c[1], c[2], p.f32("strength")],
                b: [
                    p.f32("radius").max(0.0),
                    p.f32("softening").max(1e-6),
                    0.0,
                    0.0,
                ],
                mode: menu_index(ATTRACT_MODES, p.str("mode")),
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let d = params.a.xyz - P[i].xyz;
    let r = length(d);
    var f = 0.0;
    if (r > 1e-6) {
        var m = r;
        if (params.mode == 1u) { m = 1.0; }
        if (params.mode == 2u) { m = 1.0 / max(r * r, params.b.y); }
        f = params.a.w * m * atd_falloff(r, params.b.x) / r;
    }
    v[i] = vec4f(v[i].xyz + d * (f * params.dt), v[i].w);
}",
        budget_us: 60,
    },
    Spec {
        id: "vortex",
        label: "Vortex",
        summary: "A swirl about an axis on the velocity `v` (reading `P`): spins points around the axis, optionally pulls them in toward it and lifts them along it, fading out with distance from the axis.",
        target: Target::Velocity,
        params: || {
            vec![
                ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("A point on the axis."),
                ParamSchema::vec("axis", "Axis", &[0.0, 0.0, 1.0])
                    .help("Direction of the axis (spin is counter-clockwise looking down it)."),
                ParamSchema::float("strength", "Strength", 1.0, None, None)
                    .help("Tangential acceleration (negative spins the other way)."),
                ParamSchema::float("pull", "Pull", 0.0, None, None)
                    .help("Acceleration toward the axis (negative pushes out)."),
                ParamSchema::float("lift", "Lift", 0.0, None, None)
                    .help("Acceleration along the axis."),
                ParamSchema::float("radius", "Radius", 0.0, Some(0.0), None).help(FALLOFF_HELP),
            ]
        },
        pack: |p| {
            let c = vec3(p, "centre");
            let a = unit(p, "axis");
            Packed {
                a: [c[0], c[1], c[2], p.f32("strength")],
                b: [a[0], a[1], a[2], p.f32("pull")],
                c: [p.f32("lift"), p.f32("radius").max(0.0), 0.0, 0.0],
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let q = P[i].xyz - params.a.xyz;
    let ax = params.b.xyz;
    let radial = q - ax * dot(q, ax);
    let r = length(radial);
    var acc = vec3f(0.0);
    if (r > 1e-6) {
        let k = atd_falloff(r, params.c.y);
        acc = (cross(ax, radial) / r * params.a.w - radial / r * params.b.w + ax * params.c.x) * k;
    }
    v[i] = vec4f(v[i].xyz + acc * params.dt, v[i].w);
}",
        budget_us: 60,
    },
    Spec {
        id: "drag",
        label: "Drag",
        summary: "Slows the velocity `v` down: linear drag (a fraction of the speed lost per second, frame-rate independent) or quadratic drag (stronger at high speed).",
        target: Target::Velocity,
        params: || {
            vec![
                ParamSchema::float("amount", "Amount", 1.0, Some(0.0), None).help("linear: decay rate per second (1: about 63% of the speed lost per second); quadratic: the coefficient of speed²."),
                ParamSchema::menu("mode", "Mode", DRAG_MODES, "linear").help("linear: v × e^(−amount·dt); quadratic: v / (1 + amount·|v|·dt)."),
            ]
        },
        pack: |p| Packed {
            a: [p.f32("amount").max(0.0), 0.0, 0.0, 0.0],
            mode: menu_index(DRAG_MODES, p.str("mode")),
            ..Packed::default()
        },
        body: "fn point(i: u32) {
    let vel = v[i].xyz;
    var k = exp(-params.a.x * params.dt);
    if (params.mode == 1u) { k = 1.0 / (1.0 + params.a.x * length(vel) * params.dt); }
    v[i] = vec4f(vel * k, v[i].w);
}",
        budget_us: 40,
    },
    Spec {
        id: "gravity",
        label: "Gravity",
        summary: "A constant acceleration on the velocity `v` (created at zero when missing).",
        target: Target::Velocity,
        params: || {
            vec![
                ParamSchema::vec("acceleration", "Acceleration", &[0.0, -9.8, 0.0])
                    .help("Units per second² (Earth's gravity in metres: [0, -9.8, 0])."),
            ]
        },
        pack: |p| Packed {
            a: vec3(p, "acceleration"),
            ..Packed::default()
        },
        body: "fn point(i: u32) {
    v[i] = vec4f(v[i].xyz + params.a.xyz * params.dt, v[i].w);
}",
        budget_us: 40,
    },
    Spec {
        id: "wind",
        label: "Wind",
        summary: "Air moving at a velocity, with gusts from curl noise: the velocity `v` relaxes toward the wind's at the response rate (reading `P` for the gusts).",
        target: Target::Velocity,
        params: || {
            vec![
                ParamSchema::vec("velocity", "Velocity", &[1.0, 0.0, 0.0]).help("The wind's velocity (units per second)."),
                ParamSchema::float("response", "Response", 1.0, Some(0.0), None).help("How fast points pick up the wind's velocity (per second)."),
                ParamSchema::float("gust", "Gust", 0.5, None, None).help("Strength of the gusts (units per second)."),
                ParamSchema::float("gust_frequency", "Gust Frequency", 1.0, Some(0.0), None).help("Noise cells per unit of the gust field."),
                ParamSchema::float("evolution", "Evolution", 0.0, None, None).help("Moves the gust field through noise space; drive it from time (for example `time * 0.5`) to animate."),
                seed_param(),
            ]
        },
        pack: |p| {
            let w = vec3(p, "velocity");
            Packed {
                a: [w[0], w[1], w[2], p.f32("response").max(0.0)],
                b: [
                    p.f32("gust"),
                    p.f32("gust_frequency").max(0.0),
                    p.f32("evolution"),
                    0.0,
                ],
                seed: kit::seed(p),
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let q = P[i].xyz * params.b.y + vec3f(params.b.z * 0.31, -params.b.z * 0.17, params.b.z);
    let air = params.a.xyz + atd_curl(q, params.seed) * params.b.x;
    let vel = v[i].xyz;
    let k = 1.0 - exp(-params.a.w * params.dt);
    v[i] = vec4f(vel + (air - vel) * k, v[i].w);
}",
        budget_us: 80,
    },
    Spec {
        id: "turbulence",
        label: "Turbulence",
        summary: "Fractal curl noise (several octaves of the curl noise `pnt.noise` uses) as a force on the velocity `v`: swirls at several scales, divergence-free, so points mix without bunching up.",
        target: Target::Velocity,
        params: || {
            vec![
                ParamSchema::float("amplitude", "Amplitude", 1.0, None, None).help("Acceleration of the first octave (units per second²); each further octave has half."),
                ParamSchema::float("frequency", "Frequency", 1.0, Some(0.0), None).help("Noise cells per unit of the first octave; each further octave doubles it."),
                ParamSchema::int("octaves", "Octaves", 3, Some(1), Some(8)).help("Number of octaves summed."),
                ParamSchema::vec("offset", "Offset", &[0.0, 0.0, 0.0]).help("Moves the field (in noise cells)."),
                ParamSchema::float("evolution", "Evolution", 0.0, None, None).help("Moves the field through noise space; drive it from time to animate."),
                seed_param(),
            ]
        },
        pack: |p| {
            let o = vec3(p, "offset");
            let ev = p.f32("evolution");
            Packed {
                a: [p.f32("amplitude"), p.f32("frequency").max(0.0), 0.0, 0.0],
                b: [o[0] + ev * 0.31, o[1] - ev * 0.17, o[2] + ev, 0.0],
                seed: kit::seed(p),
                extra: p.i64("octaves").clamp(1, 8) as u32,
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let q = P[i].xyz * params.a.y + params.b.xyz;
    let f = atd_fbm_curl(q, params.seed, params.extra) * params.a.x;
    v[i] = vec4f(v[i].xyz + f * params.dt, v[i].w);
}",
        budget_us: 150,
    },
    Spec {
        id: "collide",
        label: "Collide",
        summary: "Keeps points out of a plane or a sphere (or inside a sphere): a point that has passed the surface is moved back onto it and its velocity `v` is reflected, scaled by Bounce, with Friction taking off sliding speed. Writes `P` and `v` (created at zero when missing).",
        target: Target::Both,
        params: || {
            vec![
                ParamSchema::menu("shape", "Shape", COLLIDE_SHAPES, "plane").help("plane: the side the normal points to is free; sphere_outside: points stay outside the ball; sphere_inside: points stay inside it."),
                ParamSchema::vec("centre", "Centre", &[0.0, -1.0, 0.0]).help("A point on the plane, or the sphere's centre."),
                ParamSchema::vec("normal", "Normal", &[0.0, 1.0, 0.0]).help("The plane's normal (toward the free side)."),
                ParamSchema::float("radius", "Radius", 1.0, Some(0.0), None).help("The sphere's radius."),
                ParamSchema::float("bounce", "Bounce", 0.5, Some(0.0), None).help("Fraction of the normal speed kept after a hit (0: no bounce, 1: elastic)."),
                ParamSchema::float("friction", "Friction", 0.1, Some(0.0), Some(1.0)).help("Fraction of the sliding speed lost at a hit."),
            ]
        },
        pack: |p| {
            let c = vec3(p, "centre");
            Packed {
                a: [c[0], c[1], c[2], p.f32("radius").max(0.0)],
                b: unit(p, "normal"),
                c: [
                    p.f32("bounce").max(0.0),
                    p.f32("friction").clamp(0.0, 1.0),
                    0.0,
                    0.0,
                ],
                mode: menu_index(COLLIDE_SHAPES, p.str("shape")),
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    var q = P[i].xyz;
    var vel = v[i].xyz;
    var n = params.b.xyz;
    var depth = -dot(q - params.a.xyz, n);
    if (params.mode != 0u) {
        let d = q - params.a.xyz;
        let r = length(d);
        var dir = vec3f(0.0, 1.0, 0.0);
        if (r > 1e-6) { dir = d / r; }
        if (params.mode == 1u) {
            n = dir;
            depth = params.a.w - r;
        } else {
            n = -dir;
            depth = r - params.a.w;
        }
    }
    if (depth > 0.0) {
        q = q + n * depth;
        let vn = dot(vel, n);
        if (vn < 0.0) {
            let vt = vel - n * vn;
            vel = vt * (1.0 - params.c.y) - n * (vn * params.c.x);
        }
        P[i] = vec4f(q, P[i].w);
        v[i] = vec4f(vel, v[i].w);
    }
}",
        budget_us: 60,
    },
    Spec {
        id: "integrate",
        label: "Integrate",
        summary: "Moves points by their velocity for one frame: `P += v × dt × scale`. The step of a home-made simulation (Solver, or a feedback loop without Particles); Particles integrates by itself.",
        target: Target::Both,
        params: || {
            vec![
                ParamSchema::float("scale", "Scale", 1.0, None, None)
                    .help("Multiplies the step (0 freezes, negative runs backwards)."),
            ]
        },
        pack: |p| Packed {
            a: [p.f32("scale"), 0.0, 0.0, 0.0],
            ..Packed::default()
        },
        body: "fn point(i: u32) {
    P[i] = vec4f(P[i].xyz + v[i].xyz * (params.dt * params.a.x), P[i].w);
}",
        budget_us: 40,
    },
    Spec {
        id: "bend",
        label: "Bend",
        summary: "Bends `P` around an arc: the stretch of Length along Axis from Origin curves toward Direction through Angle degrees; beyond it points continue straight along the bent end.",
        target: Target::Position,
        params: || {
            vec![
                ParamSchema::vec("origin", "Origin", &[0.0, -1.0, 0.0])
                    .help("Where the bend starts."),
                ParamSchema::vec("axis", "Axis", &[0.0, 1.0, 0.0])
                    .help("Direction along which the bend runs."),
                ParamSchema::vec("direction", "Direction", &[1.0, 0.0, 0.0])
                    .help("Direction the shape bends toward (made perpendicular to the axis)."),
                ParamSchema::float("angle", "Angle", 90.0, None, None)
                    .help("Total bend over the length, in degrees."),
                ParamSchema::float("length", "Length", 2.0, Some(1e-6), None)
                    .help("Length of the bent stretch."),
            ]
        },
        pack: |p| {
            let o = vec3(p, "origin");
            let a = unit(p, "axis");
            let d = vec3(p, "direction");
            // The bend direction, perpendicular to the axis.
            let k = d[0] * a[0] + d[1] * a[1] + d[2] * a[2];
            let mut e = [d[0] - a[0] * k, d[1] - a[1] * k, d[2] - a[2] * k];
            let len = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
            e = if len < 1e-6 {
                // Any perpendicular.
                let t = if a[0].abs() < 0.9 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                let k = t[0] * a[0] + t[1] * a[1] + t[2] * a[2];
                let e = [t[0] - a[0] * k, t[1] - a[1] * k, t[2] - a[2] * k];
                let l = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
                [e[0] / l, e[1] / l, e[2] / l]
            } else {
                [e[0] / len, e[1] / len, e[2] / len]
            };
            Packed {
                a: o,
                b: [a[0], a[1], a[2], p.f32("angle").to_radians()],
                c: [e[0], e[1], e[2], p.f32("length").max(1e-6)],
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let total = params.b.w;
    let len = params.c.w;
    if (abs(total) < 1e-6) { return; }
    let A = params.b.xyz;
    let D = params.c.xyz;
    let rel = P[i].xyz - params.a.xyz;
    let s = dot(rel, A);
    let u = dot(rel, D);
    let w = rel - A * s - D * u;
    let R = len / total;
    let sc = clamp(s, 0.0, len);
    let th = sc / R;
    let extra = s - sc;
    let along = (R - u) * sin(th) + extra * cos(th);
    let across = R - (R - u) * cos(th) + extra * sin(th);
    P[i] = vec4f(params.a.xyz + w + A * along + D * across, P[i].w);
}",
        budget_us: 60,
    },
    Spec {
        id: "twist",
        label: "Twist",
        summary: "Twists `P` about an axis: points turn by an angle that grows along the axis, Angle degrees per Length.",
        target: Target::Position,
        params: || {
            vec![
                ParamSchema::vec("origin", "Origin", &[0.0, 0.0, 0.0])
                    .help("A point on the axis (no rotation there)."),
                ParamSchema::vec("axis", "Axis", &[0.0, 1.0, 0.0]).help("Direction of the axis."),
                ParamSchema::float("angle", "Angle", 90.0, None, None)
                    .help("Rotation in degrees at one Length from the origin."),
                ParamSchema::float("length", "Length", 1.0, Some(1e-6), None)
                    .help("Distance along the axis over which the rotation reaches Angle."),
            ]
        },
        pack: |p| {
            let o = vec3(p, "origin");
            let a = unit(p, "axis");
            Packed {
                a: o,
                b: [
                    a[0],
                    a[1],
                    a[2],
                    p.f32("angle").to_radians() / p.f32("length").max(1e-6),
                ],
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let rel = P[i].xyz - params.a.xyz;
    let A = params.b.xyz;
    let th = dot(rel, A) * params.b.w;
    P[i] = vec4f(params.a.xyz + atd_rotate(rel, A, th), P[i].w);
}",
        budget_us: 60,
    },
    Spec {
        id: "taper",
        label: "Taper",
        summary: "Scales `P` across an axis by an amount that changes along it: Start at the origin to End at one Length along the axis (held beyond).",
        target: Target::Position,
        params: || {
            vec![
                ParamSchema::vec("origin", "Origin", &[0.0, -1.0, 0.0])
                    .help("Where the taper starts."),
                ParamSchema::vec("axis", "Axis", &[0.0, 1.0, 0.0]).help("Direction of the taper."),
                ParamSchema::float("start", "Start", 1.0, None, None)
                    .help("Scale across the axis at the origin."),
                ParamSchema::float("end", "End", 0.0, None, None)
                    .help("Scale across the axis at one Length along it."),
                ParamSchema::float("length", "Length", 2.0, Some(1e-6), None)
                    .help("Distance over which the scale goes from Start to End."),
            ]
        },
        pack: |p| {
            let o = vec3(p, "origin");
            let a = unit(p, "axis");
            Packed {
                a: o,
                b: [a[0], a[1], a[2], p.f32("length").max(1e-6)],
                c: [p.f32("start"), p.f32("end"), 0.0, 0.0],
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let rel = P[i].xyz - params.a.xyz;
    let A = params.b.xyz;
    let s = dot(rel, A);
    let k = mix(params.c.x, params.c.y, clamp(s / params.b.w, 0.0, 1.0));
    P[i] = vec4f(params.a.xyz + A * s + (rel - A * s) * k, P[i].w);
}",
        budget_us: 60,
    },
    Spec {
        id: "wave",
        label: "Wave",
        summary: "Displaces `P` by a sine wave: along Direction, with the wave travelling along Travel (linear) or outward from Origin in the plane across Travel (radial, ripples). Animate Phase (degrees) to make it move.",
        target: Target::Position,
        params: || {
            vec![
                ParamSchema::menu("mode", "Mode", WAVE_MODES, "linear").help("linear: crests are planes across Travel; radial: rings around the Travel axis through Origin."),
                ParamSchema::vec("origin", "Origin", &[0.0, 0.0, 0.0]).help("Where the phase is measured from."),
                ParamSchema::vec("travel", "Travel", &[1.0, 0.0, 0.0]).help("linear: direction the wave travels; radial: the axis of the rings."),
                ParamSchema::vec("direction", "Direction", &[0.0, 1.0, 0.0]).help("Direction points are displaced in."),
                ParamSchema::float("amplitude", "Amplitude", 0.1, None, None).help("Largest displacement."),
                ParamSchema::float("wavelength", "Wavelength", 1.0, Some(1e-6), None).help("Distance from crest to crest."),
                ParamSchema::float("phase", "Phase", 0.0, None, None).help("Shift of the wave in degrees (for example the expression `time * 180`)."),
                ParamSchema::float("decay", "Decay", 0.0, Some(0.0), None).help("Amplitude lost per unit of distance from the origin (exponential; 0 keeps it)."),
            ]
        },
        pack: |p| {
            let o = vec3(p, "origin");
            let t = unit(p, "travel");
            let d = vec3(p, "direction");
            Packed {
                a: [o[0], o[1], o[2], p.f32("amplitude")],
                b: [
                    t[0],
                    t[1],
                    t[2],
                    std::f32::consts::TAU / p.f32("wavelength").max(1e-6),
                ],
                c: [d[0], d[1], d[2], p.f32("phase").to_radians()],
                d: [p.f32("decay").max(0.0), 0.0, 0.0, 0.0],
                mode: menu_index(WAVE_MODES, p.str("mode")),
                ..Packed::default()
            }
        },
        body: "fn point(i: u32) {
    let rel = P[i].xyz - params.a.xyz;
    let T = params.b.xyz;
    var x = dot(rel, T);
    if (params.mode == 1u) { x = length(rel - T * dot(rel, T)); }
    let amp = params.a.w * exp(-params.d.x * abs(x));
    P[i] = vec4f(P[i].xyz + params.c.xyz * (amp * sin(x * params.b.w - params.c.w)), P[i].w);
}",
        budget_us: 60,
    },
];

fn spec(id: &str) -> &'static Spec {
    SPECS
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("no field operator `{id}`"))
}

/// The schemas of every field operator.
pub fn schemas() -> Vec<OperatorSchema> {
    SPECS
        .iter()
        .map(|s| {
            crate::pnt(
                s.id,
                s.label,
                s.summary,
                &[("in", false)],
                (s.params)(),
                s.budget_us,
            )
        })
        .collect()
}

/// Short names (`attract`, …) of every field operator.
pub fn ids() -> impl Iterator<Item = &'static str> {
    SPECS.iter().map(|s| s.id)
}

/// A field operator instance of `id` (its short name, such as `attract`).
pub fn create(device: &wgpu::Device, id: &str) -> Box<dyn GeoOp> {
    let spec = spec(id);
    let (binds, decls) = match spec.target {
        Target::Velocity => (
            &[Bind::ReadWrite, Bind::Read][..],
            "@group(0) @binding(2) var<storage, read_write> v: array<vec4f>;
@group(0) @binding(3) var<storage, read> P: array<vec4f>;
",
        ),
        Target::Position => (
            &[Bind::ReadWrite][..],
            "@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
",
        ),
        Target::Both => (
            &[Bind::ReadWrite, Bind::ReadWrite][..],
            "@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> v: array<vec4f>;
",
        ),
    };
    let label = format!("pnt.{}", spec.id);
    Box::new(Field {
        spec,
        kernel: PointKernel::new(
            device,
            &label,
            &format!("{DECL}{decls}{}\n", spec.body),
            binds,
            80,
        ),
    })
}

struct Field {
    spec: &'static Spec,
    kernel: PointKernel,
}

impl GeoOp for Field {
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
        let v = (self.spec.pack)(&ctx.params);
        let mut u = Packer::<80>::new();
        u.vec4(v.a)
            .vec4(v.b)
            .vec4(v.c)
            .vec4(v.d)
            .f32(ctx.dt as f32)
            .u32(v.seed)
            .u32(v.mode)
            .u32(v.extra);
        match self.spec.target {
            Target::Velocity => {
                let pos = kit::readable(ctx, &g, names::P, AttrType::VEC3)?;
                let vel = kit::writable(ctx, &mut g, names::V, AttrType::VEC3, Missing::Zero)?;
                self.kernel.run(ctx, &hdr, &[&vel, &pos], u.bytes());
            }
            Target::Position => {
                let pos = kit::writable(ctx, &mut g, names::P, AttrType::VEC3, Missing::Error)?;
                self.kernel.run(ctx, &hdr, &[&pos], u.bytes());
            }
            Target::Both => {
                let pos = kit::writable(ctx, &mut g, names::P, AttrType::VEC3, Missing::Error)?;
                let vel = kit::writable(ctx, &mut g, names::V, AttrType::VEC3, Missing::Zero)?;
                self.kernel.run(ctx, &hdr, &[&pos, &vel], u.bytes());
            }
        }
        *ctx.output = g;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_kernel_is_valid_wgsl() {
        for s in SPECS {
            let decls = match s.target {
                Target::Velocity => {
                    "@group(0) @binding(2) var<storage, read_write> v: array<vec4f>;
@group(0) @binding(3) var<storage, read> P: array<vec4f>;"
                }
                Target::Position => {
                    "@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;"
                }
                Target::Both => {
                    "@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> v: array<vec4f>;"
                }
            };
            let src = format!(
                "{}{DECL}{decls}\n{}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
@compute @workgroup_size(256) fn main(@builtin(global_invocation_id) gid: vec3u) {{ if (gid.x < hdr.count) {{ point(gid.x); }} }}",
                kit::prelude(),
                s.body
            );
            let module = naga::front::wgsl::parse_str(&src)
                .unwrap_or_else(|e| panic!("{}: {}", s.id, e.emit_to_string(&src)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::default(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}: {e:?}", s.id));
        }
    }
}
