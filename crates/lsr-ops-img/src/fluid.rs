//! Fluid 2D (PRD §6, IMG solvers): incompressible smoke-like fluid after
//! Stam's "Stable Fluids" (semi-Lagrangian advection, Jacobi pressure
//! projection), on half-float textures at a fraction of the output size.
//!
//! Each frame: inject velocity and dye (a built-in emitter, plus the
//! optional `dye` and `velocity` images), advect velocity, remove its
//! divergence with the pressure solve, then advect the dye and show it.
//! Velocities are in grid cells per second, y down (uv convention).

use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{CookCtx, GpuOp};
use lsr_gpu::fullscreen::FullscreenPass;

use crate::ops::img;
use crate::stateful::{PingPong, STATE, clear, texture};

pub fn schema() -> OperatorSchema {
    let mut s = img(
        "fluid",
        "Fluid 2D",
        "An incompressible 2D fluid carrying coloured dye. A built-in emitter and the `dye` and `velocity` images stir it; Reset clears it.",
        &[("dye", false), ("velocity", false)],
        vec![
            ParamSchema::float("detail", "Detail", 0.5, Some(0.1), Some(1.0)).help(
                "Simulation grid size as a fraction of the output (lower is faster and smoother).",
            ),
            ParamSchema::int("iterations", "Pressure Iterations", 20, Some(1), Some(80)).help(
                "Pressure solver steps per frame; more keeps the fluid closer to incompressible.",
            ),
            ParamSchema::float(
                "velocity_dissipation",
                "Velocity Fade",
                0.2,
                Some(0.0),
                None,
            )
            .help("How fast motion dies away, per second."),
            ParamSchema::float("dye_dissipation", "Dye Fade", 0.3, Some(0.0), None)
                .help("How fast dye fades, per second."),
            ParamSchema::toggle("emitter", "Emitter", true)
                .help("A round source of dye and motion."),
            ParamSchema::vec("emitter_position", "Emitter Position", &[0.5, 0.2])
                .help("Centre as a fraction of the image from the bottom-left."),
            ParamSchema::float("emitter_radius", "Emitter Radius", 0.05, Some(0.0), None)
                .help("Radius as a fraction of the image height."),
            ParamSchema::vec("emitter_velocity", "Emitter Velocity", &[0.0, 0.5])
                .help("Push in image heights per second; +y is up."),
            ParamSchema::colour("emitter_colour", "Emitter Colour", [1.0, 0.4, 0.1, 1.0])
                .help("Dye the emitter adds."),
            ParamSchema::float("input_strength", "Input Strength", 1.0, Some(0.0), None).help(
                "Scale for dye from `dye` and motion from `velocity` (rg mapped to −1…1, +y up).",
            ),
            ParamSchema::toggle("reset", "Reset", false).help("While on, clears the fluid."),
        ],
        2500,
    );
    s.time_dependent = true;
    s
}

// v0 = (px.x, px.y, dt, aspect), v1 = (emitter uv.x, uv.y, radius uv-y, on),
// v2 = (emitter velocity cells/s x, y, input strength, cells per uv-y),
// v3 = emitter colour, v4 = (velocity fade, dye fade, 0, 0).
const INJECT_VELOCITY: &str = r#"
    let v = textureSampleLevel(in0, samp, uv, 0.0).xy;
    let dt = params.v[0].z;
    var add = vec2f(0.0);
    let d = (uv - params.v[1].xy) * vec2f(params.v[0].w, 1.0);
    if (params.v[1].w > 0.5 && length(d) < params.v[1].z) {
        add = params.v[2].xy;
    }
    // Input velocity image: rg 0…1 → −1…1 image heights per second, +y up.
    let iv = textureSampleLevel(in1, samp, uv, 0.0);
    let push = (iv.xy * 2.0 - 1.0) * vec2f(1.0, -1.0) * select(0.0, 1.0, iv.a > 0.0);
    add += push * params.v[2].z * params.v[2].w;
    return vec4f(v + add * dt * 10.0, 0.0, 1.0);"#;

const INJECT_DYE: &str = r#"
    let c = textureSampleLevel(in0, samp, uv, 0.0);
    let dt = params.v[0].z;
    var add = vec3f(0.0);
    let d = (uv - params.v[1].xy) * vec2f(params.v[0].w, 1.0);
    if (params.v[1].w > 0.5 && length(d) < params.v[1].z) {
        add = params.v[3].rgb;
    }
    let src = textureSampleLevel(in1, samp, uv, 0.0);
    add += src.rgb * src.a * params.v[2].z;
    return vec4f(c.rgb + add * dt * 3.0, 1.0);"#;

const ADVECT: &str = r#"
    // in0: the field to move, in1: velocity (cells/s).
    let px = params.v[0].xy;
    let dt = params.v[0].z;
    let v = textureSampleLevel(in1, samp, uv, 0.0).xy;
    let back = uv - v * px * dt;
    let fade = 1.0 / (1.0 + params.v[1].x * dt);
    return vec4f(textureSampleLevel(in0, samp, back, 0.0).rgb * fade, 1.0);"#;

const DIVERGENCE: &str = r#"
    let px = params.v[0].xy;
    let l = textureSampleLevel(in0, samp, uv - vec2f(px.x, 0.0), 0.0).x;
    let r = textureSampleLevel(in0, samp, uv + vec2f(px.x, 0.0), 0.0).x;
    let t = textureSampleLevel(in0, samp, uv - vec2f(0.0, px.y), 0.0).y;
    let b = textureSampleLevel(in0, samp, uv + vec2f(0.0, px.y), 0.0).y;
    return vec4f(0.5 * (r - l + b - t), 0.0, 0.0, 1.0);"#;

const JACOBI: &str = r#"
    // in0: pressure, in1: divergence.
    let px = params.v[0].xy;
    let l = textureSampleLevel(in0, samp, uv - vec2f(px.x, 0.0), 0.0).x;
    let r = textureSampleLevel(in0, samp, uv + vec2f(px.x, 0.0), 0.0).x;
    let t = textureSampleLevel(in0, samp, uv - vec2f(0.0, px.y), 0.0).x;
    let b = textureSampleLevel(in0, samp, uv + vec2f(0.0, px.y), 0.0).x;
    let div = textureSampleLevel(in1, samp, uv, 0.0).x;
    return vec4f((l + r + t + b - div) * 0.25, 0.0, 0.0, 1.0);"#;

const GRADIENT: &str = r#"
    // in0: velocity, in1: pressure.
    let px = params.v[0].xy;
    let l = textureSampleLevel(in1, samp, uv - vec2f(px.x, 0.0), 0.0).x;
    let r = textureSampleLevel(in1, samp, uv + vec2f(px.x, 0.0), 0.0).x;
    let t = textureSampleLevel(in1, samp, uv - vec2f(0.0, px.y), 0.0).x;
    let b = textureSampleLevel(in1, samp, uv + vec2f(0.0, px.y), 0.0).x;
    let v = textureSampleLevel(in0, samp, uv, 0.0).xy;
    return vec4f(v - 0.5 * vec2f(r - l, b - t), 0.0, 1.0);"#;

const SHOW: &str = r#"
    let c = textureSampleLevel(in0, samp, uv, 0.0).rgb;
    return vec4f(min(c, vec3f(1.0)), 1.0);"#;

struct State {
    velocity: PingPong,
    dye: PingPong,
    pressure: PingPong,
    divergence: wgpu::Texture,
}

pub struct Fluid {
    inject_velocity: FullscreenPass,
    inject_dye: FullscreenPass,
    advect_velocity: FullscreenPass,
    advect_dye: FullscreenPass,
    divergence: FullscreenPass,
    jacobi: FullscreenPass,
    gradient: FullscreenPass,
    show: FullscreenPass,
    state: Option<State>,
    last_time: Option<f64>,
}

impl std::fmt::Debug for Fluid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fluid").finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    let pass = |label: &str, inputs: usize, body: &str, fmt: wgpu::TextureFormat| {
        FullscreenPass::new(device, label, fmt, inputs, "", body)
    };
    Box::new(Fluid {
        inject_velocity: pass("img.fluid.inject_velocity", 2, INJECT_VELOCITY, STATE),
        inject_dye: pass("img.fluid.inject_dye", 2, INJECT_DYE, STATE),
        advect_velocity: pass("img.fluid.advect_velocity", 2, ADVECT, STATE),
        advect_dye: pass("img.fluid.advect_dye", 2, ADVECT, STATE),
        divergence: pass("img.fluid.divergence", 1, DIVERGENCE, STATE),
        jacobi: pass("img.fluid.jacobi", 2, JACOBI, STATE),
        gradient: pass("img.fluid.gradient", 2, GRADIENT, STATE),
        show: pass("img.fluid.show", 1, SHOW, format),
        state: None,
        last_time: None,
    })
}

impl GpuOp for Fluid {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out = ctx.output;
        let detail = p.f32("detail").clamp(0.1, 1.0);
        let (w, h) = (
            ((out.width() as f32 * detail).round() as u32).max(8),
            ((out.height() as f32 * detail).round() as u32).max(8),
        );
        let fits = self
            .state
            .as_ref()
            .is_some_and(|s| s.divergence.width() == w && s.divergence.height() == h);
        if p.bool("reset") || !fits {
            let st = State {
                velocity: PingPong::new(ctx.device, w, h),
                dye: PingPong::new(ctx.device, w, h),
                pressure: PingPong::new(ctx.device, w, h),
                divergence: texture(ctx.device, w, h, STATE, "lsr-fluid-divergence"),
            };
            for t in [
                st.velocity.read(),
                st.velocity.write(),
                st.dye.read(),
                st.dye.write(),
                st.pressure.read(),
                st.pressure.write(),
            ] {
                clear(ctx.encoder, t);
            }
            self.state = Some(st);
        }
        let dt = self
            .last_time
            .map_or(1.0 / 60.0, |t| (ctx.time - t).clamp(0.0, 1.0 / 20.0));
        // Offline every step is the same length (FR-H1).
        let dt = p.mode().step().map_or(dt, |s| s.min(1.0 / 20.0)) as f32;
        self.last_time = Some(ctx.time);
        let st = self.state.as_mut().expect("created above");

        let pos = p.vec4("emitter_position");
        let vel = p.vec4("emitter_velocity");
        let cells_per_height = h as f32;
        let params = [
            [1.0 / w as f32, 1.0 / h as f32, dt, w as f32 / h as f32],
            [
                pos[0],
                1.0 - pos[1],
                p.f32("emitter_radius"),
                f32::from(u8::from(p.bool("emitter"))),
            ],
            [
                vel[0] * cells_per_height,
                -vel[1] * cells_per_height,
                p.f32("input_strength"),
                cells_per_height,
            ],
            p.vec4("emitter_colour"),
        ];
        let fade = |f: f32| {
            let mut ps = params;
            ps[1] = [f, 0.0, 0.0, 0.0];
            ps
        };
        let (dye_in, vel_in) = (ctx.input_or_black(0).clone(), ctx.input_or_black(1).clone());
        let (d, q, e) = (ctx.device, ctx.queue, &mut *ctx.encoder);

        self.inject_velocity.draw(
            d,
            q,
            e,
            &[st.velocity.read(), &vel_in],
            &params,
            st.velocity.write(),
        );
        st.velocity.swap();
        self.inject_dye
            .draw(d, q, e, &[st.dye.read(), &dye_in], &params, st.dye.write());
        st.dye.swap();
        self.advect_velocity.draw(
            d,
            q,
            e,
            &[st.velocity.read(), st.velocity.read()],
            &fade(p.f32("velocity_dissipation")),
            st.velocity.write(),
        );
        st.velocity.swap();
        self.divergence
            .draw(d, q, e, &[st.velocity.read()], &params, &st.divergence);
        for _ in 0..p.i64("iterations").clamp(1, 80) {
            self.jacobi.draw(
                d,
                q,
                e,
                &[st.pressure.read(), &st.divergence],
                &params,
                st.pressure.write(),
            );
            st.pressure.swap();
        }
        self.gradient.draw(
            d,
            q,
            e,
            &[st.velocity.read(), st.pressure.read()],
            &params,
            st.velocity.write(),
        );
        st.velocity.swap();
        self.advect_dye.draw(
            d,
            q,
            e,
            &[st.dye.read(), st.velocity.read()],
            &fade(p.f32("dye_dissipation")),
            st.dye.write(),
        );
        st.dye.swap();
        self.show.draw(d, q, e, &[st.dye.read()], &[], out);
        Ok(())
    }
}
