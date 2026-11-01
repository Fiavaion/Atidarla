//! Resolution (PRD §6, IMG transform & time): resamples the input to
//! another size. Operators downstream follow the new size (an operator's
//! output takes its first input's size unless it chooses otherwise), so
//! a chain can run at a fraction of the project resolution.

use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{CookCtx, GpuOp, Params};
use lsr_gpu::fullscreen::FullscreenPass;

use crate::ops::img;

const MODES: &[&str] = &["fraction", "pixels"];

pub fn schema() -> OperatorSchema {
    img(
        "resolution",
        "Resolution",
        "Resamples the input to another size: a fraction of the input (or of the project size when nothing is connected), or exact pixels. Operators after it work at the new size.",
        &[("in", false)],
        vec![
            ParamSchema::menu("mode", "Mode", MODES, "fraction")
                .help("`fraction` scales the input size; `pixels` sets width and height."),
            ParamSchema::float("fraction", "Fraction", 0.5, Some(0.01), Some(8.0))
                .help("Scale applied to both sides in `fraction` mode."),
            ParamSchema::int("width", "Width", 1280, Some(1), Some(16_384))
                .help("Output width in `pixels` mode."),
            ParamSchema::int("height", "Height", 720, Some(1), Some(16_384))
                .help("Output height in `pixels` mode."),
        ],
        40,
    )
}

#[derive(Debug)]
pub struct Resolution {
    pass: FullscreenPass,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Resolution {
        pass: FullscreenPass::new(
            device,
            "img.resolution",
            format,
            1,
            "",
            "return textureSampleLevel(in0, samp, uv, 0.0);",
        ),
    })
}

impl GpuOp for Resolution {
    fn output_size(
        &self,
        p: &Params<'_>,
        input: Option<(u32, u32)>,
        project: (u32, u32),
    ) -> Option<(u32, u32)> {
        Some(if p.str("mode") == "pixels" {
            (
                p.i64("width").clamp(1, 16_384) as u32,
                p.i64("height").clamp(1, 16_384) as u32,
            )
        } else {
            let (w, h) = input.unwrap_or(project);
            let f = p.f32("fraction").clamp(0.01, 8.0);
            (
                ((w as f32 * f).round() as u32).max(1),
                ((h as f32 * f).round() as u32).max(1),
            )
        })
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let input = ctx
            .inputs
            .first()
            .and_then(Option::as_ref)
            .unwrap_or(ctx.black);
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[input],
            &[],
            ctx.output,
        );
        Ok(())
    }
}
