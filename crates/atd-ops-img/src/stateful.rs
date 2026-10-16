//! Stateful IMG operators: frame history and GPU simulations. They keep
//! their own textures between cooks, so they are time-dependent.

use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;

use crate::ops::img;

/// Internal simulation state format: half floats, filterable, renderable.
pub(crate) const STATE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub(crate) fn texture(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

pub(crate) fn clear(encoder: &mut wgpu::CommandEncoder, t: &wgpu::Texture) {
    let view = t.create_view(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("atd-clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

/// Two state textures, read from one and written to the other each step.
#[derive(Debug)]
pub(crate) struct PingPong {
    tex: [wgpu::Texture; 2],
    current: usize,
}

impl PingPong {
    pub(crate) fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        Self {
            tex: [
                texture(device, w, h, STATE, "atd-state-a"),
                texture(device, w, h, STATE, "atd-state-b"),
            ],
            current: 0,
        }
    }

    pub(crate) fn matches(&self, out: &wgpu::Texture) -> bool {
        self.tex[0].width() == out.width() && self.tex[0].height() == out.height()
    }

    pub(crate) fn read(&self) -> &wgpu::Texture {
        &self.tex[self.current]
    }

    pub(crate) fn write(&self) -> &wgpu::Texture {
        &self.tex[1 - self.current]
    }

    pub(crate) fn swap(&mut self) {
        self.current = 1 - self.current;
    }
}

// ---------------------------------------------------------------- Cache

pub fn cache_schema() -> OperatorSchema {
    let mut s = img(
        "cache",
        "Cache",
        "Delays the input by a number of frames (frame history).",
        &[("in", false)],
        vec![
            ParamSchema::int("delay", "Delay", 10, Some(0), Some(120))
                .help("Frames of delay; 0 passes the input through."),
        ],
        60,
    );
    s.time_dependent = true;
    s
}

#[derive(Default)]
pub struct Cache {
    ring: Vec<wgpu::Texture>,
    head: usize,
    filled: usize,
}

impl GpuOp for Cache {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let delay = ctx.params.i64("delay").clamp(0, 120) as usize;
        let out = ctx.output;
        if self
            .ring
            .first()
            .is_some_and(|t| t.width() != out.width() || t.height() != out.height())
        {
            self.ring.clear();
            self.filled = 0;
        }
        // Grow-only ring, so changing the delay keeps the history.
        while self.ring.len() < delay + 1 {
            self.ring.push(texture(
                ctx.device,
                out.width(),
                out.height(),
                out.format(),
                "atd-cache",
            ));
        }
        let len = self.ring.len();
        self.head = (self.head + 1) % len;
        match ctx.inputs.first().and_then(Option::as_ref) {
            Some(input) => ctx.encoder.copy_texture_to_texture(
                input.as_image_copy(),
                self.ring[self.head].as_image_copy(),
                input.size(),
            ),
            None => clear(ctx.encoder, &self.ring[self.head]),
        }
        self.filled = (self.filled + 1).min(len);
        // Until enough history exists, show the oldest frame we have.
        let back = delay.min(self.filled - 1);
        let src = &self.ring[(self.head + len - back) % len];
        ctx.encoder
            .copy_texture_to_texture(src.as_image_copy(), out.as_image_copy(), out.size());
        Ok(())
    }
}

// --------------------------------------------------------------- Ripple

pub fn ripple_schema() -> OperatorSchema {
    let mut s = img(
        "ripple",
        "Ripple",
        "A water-surface simulation: bright areas of the `drive` input push the surface, and waves spread and fade.",
        &[("drive", false)],
        vec![
            ParamSchema::float("strength", "Strength", 0.3, None, None)
                .help("How strongly the drive input pushes the surface each step."),
            ParamSchema::float("damping", "Damping", 0.985, Some(0.0), Some(1.0))
                .help("Fraction of wave height kept per step."),
            ParamSchema::int("steps", "Steps", 2, Some(1), Some(16))
                .help("Simulation steps per frame (wave speed)."),
            ParamSchema::float("gain", "Gain", 2.0, None, None)
                .help("Output brightness = 0.5 + height × gain."),
            ParamSchema::toggle("reset", "Reset", false).help("While on, flattens the surface."),
        ],
        500,
    );
    s.time_dependent = true;
    s
}

const RIPPLE_STEP: &str = r#"
    let px = params.v[0].xy;
    let s = textureSampleLevel(in0, samp, uv, 0.0);
    let n = textureSampleLevel(in0, samp, uv + vec2f(px.x, 0.0), 0.0).r
          + textureSampleLevel(in0, samp, uv - vec2f(px.x, 0.0), 0.0).r
          + textureSampleLevel(in0, samp, uv + vec2f(0.0, px.y), 0.0).r
          + textureSampleLevel(in0, samp, uv - vec2f(0.0, px.y), 0.0).r;
    var h = (n * 0.5 - s.g) * params.v[0].z;
    let d = textureSampleLevel(in1, samp, uv, 0.0);
    h += dot(d.rgb, vec3f(0.2126, 0.7152, 0.0722)) * d.a * params.v[0].w;
    return vec4f(h, s.r, 0.0, 1.0);"#;

const RIPPLE_SHOW: &str = r#"
    let v = 0.5 + textureSample(in0, samp, uv).r * params.v[0].x;
    return vec4f(v, v, v, 1.0);"#;

pub struct Ripple {
    step: FullscreenPass,
    show: FullscreenPass,
    state: Option<PingPong>,
}

impl Ripple {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            step: FullscreenPass::new(device, "img.ripple.step", STATE, 2, "", RIPPLE_STEP),
            show: FullscreenPass::new(device, "img.ripple.show", format, 1, "", RIPPLE_SHOW),
            state: None,
        }
    }
}

impl GpuOp for Ripple {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out = ctx.output;
        if p.bool("reset") || !self.state.as_ref().is_some_and(|s| s.matches(out)) {
            let st = PingPong::new(ctx.device, out.width(), out.height());
            clear(ctx.encoder, &st.tex[0]);
            clear(ctx.encoder, &st.tex[1]);
            self.state = Some(st);
        }
        let st = self.state.as_mut().expect("created above");
        let drive = ctx.input_or_black(0).clone();
        let px = [1.0 / out.width() as f32, 1.0 / out.height() as f32];
        let params = [[px[0], px[1], p.f32("damping"), p.f32("strength")]];
        for _ in 0..p.i64("steps").clamp(1, 16) {
            self.step.draw(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &[st.read(), &drive],
                &params,
                st.write(),
            );
            st.swap();
        }
        self.show.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[st.read()],
            &[[p.f32("gain"), 0.0, 0.0, 0.0]],
            out,
        );
        Ok(())
    }
}

// --------------------------------------------------- Reaction-Diffusion

pub fn reaction_diffusion_schema() -> OperatorSchema {
    let mut s = img(
        "reaction_diffusion",
        "Reaction-Diffusion",
        "A Gray-Scott reaction-diffusion simulation growing organic patterns from a seed; bright areas of the `seed` input add more.",
        &[("seed", false)],
        vec![
            ParamSchema::float("feed", "Feed", 0.055, Some(0.0), Some(0.1))
                .help("Feed rate of chemical A."),
            ParamSchema::float("kill", "Kill", 0.062, Some(0.0), Some(0.1))
                .help("Removal rate of chemical B; feed/kill pairs select the pattern family."),
            ParamSchema::int("steps", "Steps", 8, Some(1), Some(64))
                .help("Simulation steps per frame (growth speed)."),
            ParamSchema::colour("colour_a", "Colour A", [0.0, 0.0, 0.0, 1.0])
                .help("Colour where B is absent."),
            ParamSchema::colour("colour_b", "Colour B", [1.0, 1.0, 1.0, 1.0])
                .help("Colour where B is present."),
            ParamSchema::toggle("reset", "Reset", false)
                .help("While on, restarts from the centre seed."),
        ],
        1500,
    );
    s.time_dependent = true;
    s
}

const RD_INIT: &str = r#"
    let d = abs(uv - vec2f(0.5));
    var b = 0.0;
    if (d.x < 0.04 && d.y < 0.04) { b = 1.0; }
    return vec4f(1.0, b, 0.0, 1.0);"#;

const RD_STEP: &str = r#"
    let px = params.v[0].xy;
    let s = textureSampleLevel(in0, samp, uv, 0.0);
    let adj = textureSampleLevel(in0, samp, uv + vec2f(px.x, 0.0), 0.0)
            + textureSampleLevel(in0, samp, uv - vec2f(px.x, 0.0), 0.0)
            + textureSampleLevel(in0, samp, uv + vec2f(0.0, px.y), 0.0)
            + textureSampleLevel(in0, samp, uv - vec2f(0.0, px.y), 0.0);
    let diag = textureSampleLevel(in0, samp, uv + px, 0.0)
             + textureSampleLevel(in0, samp, uv - px, 0.0)
             + textureSampleLevel(in0, samp, uv + vec2f(px.x, -px.y), 0.0)
             + textureSampleLevel(in0, samp, uv + vec2f(-px.x, px.y), 0.0);
    let lap = adj * 0.2 + diag * 0.05 - s;
    let a = s.r;
    let b = s.g;
    let feed = params.v[0].z;
    let kill = params.v[0].w;
    let abb = a * b * b;
    let na = a + (1.0 * lap.r - abb + feed * (1.0 - a));
    var nb = b + (0.5 * lap.g + abb - (kill + feed) * b);
    let seed = textureSampleLevel(in1, samp, uv, 0.0);
    nb = max(nb, step(0.5, dot(seed.rgb, vec3f(0.2126, 0.7152, 0.0722)) * seed.a));
    return vec4f(clamp(na, 0.0, 1.0), clamp(nb, 0.0, 1.0), 0.0, 1.0);"#;

const RD_SHOW: &str = r#"
    let b = textureSample(in0, samp, uv).g;
    return mix(params.v[0], params.v[1], clamp(b * 3.0, 0.0, 1.0));"#;

pub struct ReactionDiffusion {
    init: FullscreenPass,
    step: FullscreenPass,
    show: FullscreenPass,
    state: Option<PingPong>,
}

impl ReactionDiffusion {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            init: FullscreenPass::new(device, "img.reaction_diffusion.init", STATE, 0, "", RD_INIT),
            step: FullscreenPass::new(device, "img.reaction_diffusion.step", STATE, 2, "", RD_STEP),
            show: FullscreenPass::new(
                device,
                "img.reaction_diffusion.show",
                format,
                1,
                "",
                RD_SHOW,
            ),
            state: None,
        }
    }
}

impl GpuOp for ReactionDiffusion {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out = ctx.output;
        if p.bool("reset") || !self.state.as_ref().is_some_and(|s| s.matches(out)) {
            let st = PingPong::new(ctx.device, out.width(), out.height());
            self.init
                .draw(ctx.device, ctx.queue, ctx.encoder, &[], &[], st.read());
            self.state = Some(st);
        }
        let st = self.state.as_mut().expect("created above");
        let seed = ctx.input_or_black(0).clone();
        let px = [1.0 / out.width() as f32, 1.0 / out.height() as f32];
        let params = [[px[0], px[1], p.f32("feed"), p.f32("kill")]];
        for _ in 0..p.i64("steps").clamp(1, 64) {
            self.step.draw(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &[st.read(), &seed],
                &params,
                st.write(),
            );
            st.swap();
        }
        self.show.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[st.read()],
            &[p.vec4("colour_a"), p.vec4("colour_b")],
            out,
        );
        Ok(())
    }
}

/// Stateful operators: schema and factory.
pub type Stateful = (
    fn() -> OperatorSchema,
    fn(&wgpu::Device, wgpu::TextureFormat) -> Box<dyn GpuOp>,
);

pub static STATEFUL: &[Stateful] = &[
    (cache_schema, |_, _| Box::<Cache>::default()),
    (ripple_schema, |d, f| Box::new(Ripple::new(d, f))),
    (reaction_diffusion_schema, |d, f| {
        Box::new(ReactionDiffusion::new(d, f))
    }),
    (crate::fluid::schema, crate::fluid::create),
];
