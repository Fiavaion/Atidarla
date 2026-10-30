//! Control Prep: turns an image into a control image for ControlNet-style
//! conditioning (edges, line art, scribbles) on the GPU, without a model.
//! Depth and pose control images come from the Depth and Pose operators.

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "ai.control_prep".into(),
        family: OpFamily::Ai,
        label: "Control Prep".into(),
        summary: "Prepares a control image for diffusion conditioning, on the GPU: Canny-style edges (thin, with hysteresis), soft edges, line art (dark lines on white), scribble (thick lines) or a threshold. Feed it to the Diffusion node's `control` input.".into(),
        version: 1,
        params: vec![
            ParamSchema::menu(
                "mode",
                "Mode",
                &["canny", "soft_edge", "lineart", "scribble", "threshold"],
                "canny",
            )
            .help("`canny`: thin edges kept where strong, or weak and touching a strong one; `soft_edge`: edge strength; `lineart`: soft edges as dark lines on white; `scribble`: thick white strokes; `threshold`: white where brighter than High."),
            ParamSchema::float("low", "Low", 0.1, Some(0.0), Some(1.0))
                .help("Canny: weaker edges are dropped; edges between Low and High survive only next to a strong edge."),
            ParamSchema::float("high", "High", 0.25, Some(0.0), Some(1.0))
                .help("Canny: edges at least this strong are always kept. Threshold: the luminance cut."),
            ParamSchema::toggle("invert", "Invert", false).help("Swap black and white."),
        ],
        inputs: vec![SocketSchema {
            name: "image".into(),
            wire: WireType::Img,
            optional: false,
            feedback: false,
        }],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Img,
            optional: false,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 150,
        time_dependent: false,
        sink: false,
    }
}

const HELPERS: &str = r#"
fn lum(uv: vec2f) -> f32 {
    return dot(textureSampleLevel(in0, samp, uv, 0.0).rgb, vec3f(0.299, 0.587, 0.114));
}
fn grad(uv: vec2f, px: vec2f) -> vec2f {
    let tl = lum(uv + vec2f(-px.x, -px.y));
    let t = lum(uv + vec2f(0.0, -px.y));
    let tr = lum(uv + vec2f(px.x, -px.y));
    let l = lum(uv + vec2f(-px.x, 0.0));
    let r = lum(uv + vec2f(px.x, 0.0));
    let bl = lum(uv + vec2f(-px.x, px.y));
    let b = lum(uv + vec2f(0.0, px.y));
    let br = lum(uv + vec2f(px.x, px.y));
    let gx = (tr + 2.0 * r + br) - (tl + 2.0 * l + bl);
    let gy = (bl + 2.0 * b + br) - (tl + 2.0 * t + tr);
    return vec2f(gx, gy) * 0.25;
}
// Magnitude after non-maximum suppression along the gradient.
fn thin(uv: vec2f, px: vec2f) -> f32 {
    let g = grad(uv, px);
    let m = length(g);
    if (m < 1e-5) {
        return 0.0;
    }
    let d = normalize(g) * px;
    let a = length(grad(uv + d, px));
    let b = length(grad(uv - d, px));
    return select(0.0, m, m >= a && m >= b);
}
"#;

const BODY: &str = r#"
    let mode = params.v[0].x;
    let low = params.v[0].y;
    let high = params.v[0].z;
    let px = params.v[1].xy;
    var v = 0.0;
    if (mode < 0.5) {
        let m = thin(uv, px);
        if (m >= high) {
            v = 1.0;
        } else if (m >= low) {
            for (var j = -1; j <= 1; j++) {
                for (var i = -1; i <= 1; i++) {
                    if (thin(uv + vec2f(f32(i), f32(j)) * px, px) >= high) {
                        v = 1.0;
                    }
                }
            }
        }
    } else if (mode < 1.5) {
        v = clamp(length(grad(uv, px)) * 2.0, 0.0, 1.0);
    } else if (mode < 2.5) {
        v = 1.0 - clamp(length(grad(uv, px)) * 3.0, 0.0, 1.0);
    } else if (mode < 3.5) {
        var m = 0.0;
        for (var j = -2; j <= 2; j++) {
            for (var i = -2; i <= 2; i++) {
                m = max(m, length(grad(uv + vec2f(f32(i), f32(j)) * px, px)));
            }
        }
        v = step(high, m);
    } else {
        v = step(high, lum(uv));
    }
    if (params.v[0].w > 0.5) {
        v = 1.0 - v;
    }
    return vec4f(v, v, v, 1.0);
"#;

#[derive(Debug)]
struct ControlPrep {
    pass: FullscreenPass,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(ControlPrep {
        pass: FullscreenPass::new(device, "ai.control_prep", format, 1, HELPERS, BODY),
    })
}

impl GpuOp for ControlPrep {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = &ctx.params;
        let mode = match p.str("mode") {
            "soft_edge" => 1.0,
            "lineart" => 2.0,
            "scribble" => 3.0,
            "threshold" => 4.0,
            _ => 0.0,
        };
        let input = ctx.input_or_black(0).clone();
        let px = [1.0 / input.width() as f32, 1.0 / input.height() as f32];
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&input],
            &[
                [
                    mode,
                    p.f32("low"),
                    p.f32("high"),
                    f32::from(p.bool("invert")),
                ],
                [px[0], px[1], 0.0, 0.0],
            ],
            ctx.output,
        );
        Ok(())
    }
}
