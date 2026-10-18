//! `pnt.noise`: curl noise on the GPU, as a displacement of `P` or a force
//! on `v`. A deformer: it writes in place when the engine hands it the
//! geometry, else copy-on-write, and shares every other attribute.

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{GeoCtx, GeoOp};
use atd_gpu::geo::kernels::{Deformer, Packer};

const MODES: &[&str] = &["displace", "velocity"];

pub fn schema() -> OperatorSchema {
    crate::pnt(
        "noise",
        "Noise",
        "Curl noise, a swirling divergence-free field: displaces `P`, or adds a force to the velocity `v` (created at zero when missing) so particles flow. Animate it through `evolution` (for example the expression `time * 0.3`).",
        &[("in", false)],
        vec![
            ParamSchema::menu("mode", "Mode", MODES, "displace").help(
                "displace: P += amplitude × curl(P); velocity: v += amplitude × curl(P) × the frame's time step.",
            ),
            ParamSchema::float("amplitude", "Amplitude", 0.2, None, None)
                .help("Strength: a distance (displace) or an acceleration in units per second² (velocity)."),
            ParamSchema::float("frequency", "Frequency", 2.0, Some(0.0), None)
                .help("Spatial frequency: noise cells per unit."),
            ParamSchema::vec("offset", "Offset", &[0.0, 0.0, 0.0])
                .help("Moves the noise field (in noise cells)."),
            ParamSchema::float("evolution", "Evolution", 0.0, None, None).help(
                "Moves the field along a diagonal through noise space, so the swirls drift and change; drive it from time to animate.",
            ),
            ParamSchema::int("seed", "Seed", 0, Some(0), None).help("Selects a different field."),
        ],
        120,
    )
}

const DECL: &str = "struct Params {
    offset: vec4f,
    amplitude: f32,
    frequency: f32,
    seed: u32,
    pad: u32,
};";

/// Displace: the value is P.
const DISPLACE: &str = "let q = value.xyz * params.frequency + params.offset.xyz;
    return vec4f(value.xyz + atd_curl(q, params.seed) * params.amplitude, value.w);";

/// Velocity: the value is v, `aux` is P; `amplitude` is already × dt.
const VELOCITY: &str = "let q = aux[i].xyz * params.frequency + params.offset.xyz;
    return vec4f(value.xyz + atd_curl(q, params.seed) * params.amplitude, value.w);";

pub struct Noise {
    displace: Deformer,
    velocity: Deformer,
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Noise {
        displace: Deformer::new(device, "pnt.noise.displace", DECL, DISPLACE, 32, false),
        velocity: Deformer::new(device, "pnt.noise.velocity", DECL, VELOCITY, 32, true),
    })
}

impl GeoOp for Noise {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        let p = ctx.params;
        let velocity = p.str("mode") == "velocity";
        let amplitude = if velocity {
            p.f32("amplitude") * ctx.dt as f32
        } else {
            p.f32("amplitude")
        };
        // Evolution moves the field along a diagonal through noise space, so
        // the pattern changes rather than sliding sideways.
        let o = p.vec4("offset");
        let ev = p.f32("evolution");
        let mut u = Packer::<32>::new();
        u.vec4([o[0] + ev * 0.31, o[1] - ev * 0.17, o[2] + ev, 0.0])
            .f32(amplitude)
            .f32(p.f32("frequency"))
            .u32(p.i64("seed").max(0) as u32);
        if g.points() > 0 {
            if velocity {
                if g.point_typed(names::V, AttrType::VEC3).is_none() {
                    let v = ctx.alloc(names::V, AttrType::VEC3, g.points());
                    ctx.encoder
                        .clear_buffer(v.gpu_buffer().expect("on the GPU"), 0, None);
                    g.set_point(v);
                }
                self.velocity
                    .apply(ctx, &mut g, names::V, Some(names::P), u.bytes())?;
            } else {
                self.displace
                    .apply(ctx, &mut g, names::P, None, u.bytes())?;
            }
        }
        *ctx.output = g;
        Ok(())
    }
}
