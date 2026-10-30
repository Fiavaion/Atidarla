//! Prompt Clip (FR-A5): a Stage clip that carries a prompt for the AI nodes
//! following its deck (`prompts_from`). Wired into a layer slot it shows
//! its input (or black), so the deck stays a picture; the Stage feeds its
//! prompt, seed and LoRA to the followers' Prompt Blend with the crossfader
//! as the blend (`atd_stage::ai`).

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "ai.prompt".into(),
        family: OpFamily::Ai,
        label: "Prompt Clip".into(),
        summary: "An AI clip for the Stage: wire it into a layer slot and the AI nodes that follow the deck (their `Prompts From` names the deck's Mixer) take its prompt, seed and LoRA. The clips playing on crossfader sides A and B are blended by the crossfader. Shows its input (or black) as its picture.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("prompt", "Prompt", "a watercolour painting")
                .multiline()
                .help("The prompt this clip plays."),
            ParamSchema::string("negative", "Negative", "").help("What to steer away from while this clip leads the blend."),
            ParamSchema::int("seed", "Seed", 1, Some(0), None).help("Noise seed while this clip leads the blend."),
            ParamSchema::menu("reseed", "Reseed", &["off", "launch", "beat", "bar"], "off")
                .help("Moves the seed on: each time the clip is launched, every beat, or every bar of the deck's clock."),
            ParamSchema::path("lora", "LoRA").help("A LoRA file used while this clip leads the blend."),
            ParamSchema::float("lora_weight", "LoRA Weight", 1.0, Some(0.0), Some(2.0)).help("Strength of the LoRA."),
        ],
        inputs: vec![SocketSchema {
            name: "image".into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        }],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Img,
            optional: false,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 50,
        time_dependent: false,
        sink: false,
    }
}

#[derive(Debug)]
struct PromptClip {
    pass: FullscreenPass,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(PromptClip {
        pass: FullscreenPass::new(
            device,
            "ai.prompt",
            format,
            1,
            "",
            "return textureSample(in0, samp, uv);",
        ),
    })
}

impl GpuOp for PromptClip {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let input = ctx.input_or_black(0).clone();
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&input],
            &[],
            ctx.output,
        );
        Ok(())
    }
}
