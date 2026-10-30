//! Schemas of the AI operators (PRD §6, §7.5).

use atd_core::capability::{CapabilityRequirement, Fallback};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};

/// What an AI operator runs and how its parameters reach the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// SD-Turbo-class img2img with Prompt Blend (FR-A3, FR-A4).
    Diffusion,
    /// Any adapter and model by id or path (built-ins, ONNX, …).
    Model,
    /// An ONNX vision task (FR-A9).
    Vision(&'static str),
    /// Streaming video-to-video (FR-A11).
    Video,
}

/// Every AI operator that runs on a host: type id, label, kind.
pub const HOSTED: &[(&str, &str, Kind)] = &[
    ("ai.diffusion", "Diffusion", Kind::Diffusion),
    ("ai.model", "AI Model", Kind::Model),
    ("ai.depth", "Depth", Kind::Vision("depth")),
    ("ai.segment", "Segment", Kind::Vision("segment")),
    ("ai.pose", "Pose", Kind::Vision("pose")),
    ("ai.optical_flow", "Optical Flow", Kind::Vision("flow")),
    (
        "ai.background",
        "Background Remove",
        Kind::Vision("background"),
    ),
    ("ai.upscale", "Upscale", Kind::Vision("upscale")),
    ("ai.onnx", "ONNX Inference", Kind::Vision("generic")),
    ("ai.video_model", "Video Model", Kind::Video),
];

pub fn kind(type_id: &str) -> Option<Kind> {
    HOSTED.iter().find(|h| h.0 == type_id).map(|h| h.2)
}

/// Prompt slots of Prompt Blend (A58).
pub const PROMPTS: usize = 4;

fn img(name: &str, optional: bool) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional,
        feedback: false,
    }
}

/// Parameters every hosted AI operator has: where it runs and how the
/// Output Governor fills the frames between results (FR-A6, FR-A7).
fn common(route: &str, size: i64) -> Vec<ParamSchema> {
    vec![
        ParamSchema::string("route", "Route", route).help(
            "Where the node runs, in order of preference: `local`, `lan:<name>` and `cloud:<name>` separated by `→`, `->` or commas. The first host that is ready and can run the model wins; a host that meets the node's real-time need is preferred. Cloud hosts are skipped in Sovereign mode and once the project's spend cap is reached.",
        ),
        ParamSchema::int("width", "Width", size, Some(8), Some(2048))
            .help("Width of the frames sent to the model; the result is scaled to the output."),
        ParamSchema::int("height", "Height", size, Some(8), Some(2048))
            .help("Height of the frames sent to the model."),
        ParamSchema::menu("policy", "Governor", &["hold", "blend", "flow", "passthrough"], "blend").help(
            "How the output fills the frames between AI results, at the project frame rate: `hold` shows the newest result; `blend` cross-fades from the previous result to the newest over one AI interval; `flow` interpolates along the motion between them; `passthrough` shows the input with the effect below whenever no result is fresh.",
        ),
        ParamSchema::float("effect", "Effect", 0.5, Some(0.0), Some(1.0))
            .help("Strength of the desaturate-and-tint effect on the input shown before the first result (and when stale under `passthrough`)."),
        ParamSchema::int("in_flight", "In Flight", 1, Some(1), Some(4))
            .help("Frames sent to the host at once. More hides network latency; fewer keeps the result closer to the input."),
        ParamSchema::float("budget_ms", "Latency Budget", 0.0, Some(0.0), Some(5000.0))
            .help("When the round trip stays above this many milliseconds for 3 s, the node moves to the next host on its route; 0 turns it off."),
    ]
}

fn prompt_params() -> Vec<ParamSchema> {
    let mut v = vec![ParamSchema::string("prompts_from", "Prompts From", "").help(
        "The name of a Stage deck's Mixer: the Prompt Clips playing on its crossfader sides A and B become prompts 1 and 2, blended by the crossfader, with the leading clip's seed, negative and LoRA (AI clips). Empty: the prompts below.",
    )];
    for i in 0..PROMPTS {
        let n = i + 1;
        let default = if i == 0 { "a watercolour painting" } else { "" };
        v.push(
            ParamSchema::string(&format!("prompt{n}"), &format!("Prompt {n}"), default)
                .multiline()
                .help("A prompt of the Prompt Blend: the model sees the weighted mix of the prompts' embeddings."),
        );
        v.push(
            ParamSchema::float(
                &format!("weight{n}"),
                &format!("Weight {n}"),
                if i == 0 { 1.0 } else { 0.0 },
                Some(0.0),
                Some(1.0),
            )
            .help("This prompt's share of the blend (weights are normalised). Bind it to a channel or the crossfader to morph between prompts."),
        );
    }
    v
}

pub fn schema(type_id: &str) -> OperatorSchema {
    let (_, label, kind) = *HOSTED
        .iter()
        .find(|h| h.0 == type_id)
        .expect("a hosted AI operator");
    let mut params;
    let mut inputs = vec![img("image", false)];
    let mut capabilities = vec![];
    let (summary, budget): (String, u32) = match kind {
        Kind::Diffusion => {
            params = common("local", 512);
            params.extend([
                ParamSchema::string("model", "Model", "sd-turbo").help(
                    "Model id from the model manager (`sd-turbo`, `sdxl-turbo`), or a path to a diffusers folder. The project's commercial-safe filter applies.",
                ),
            ]);
            params.extend(prompt_params());
            params.extend([
                ParamSchema::string("negative", "Negative", "").help("What to steer away from (used by residual CFG `onetime` and full guidance)."),
                ParamSchema::int("seed", "Seed", 1, Some(0), None).help("Noise seed; change it on a beat for a new look."),
                ParamSchema::float("strength", "Strength", 0.5, Some(0.0), Some(1.0)).help("How far the result departs from the input (img2img denoising strength)."),
                ParamSchema::int("steps", "Steps", 1, Some(1), Some(4)).help("Denoising steps; each adds one stage to the stream batch."),
                ParamSchema::float("guidance", "Guidance", 1.0, Some(0.0), Some(10.0)).help("Classifier-free guidance scale; 1 needs no negative pass."),
                ParamSchema::menu("rcfg", "Residual CFG", &["none", "self", "onetime"], "self").help("Residual classifier-free guidance (StreamDiffusion): `self` derives the negative from the input at no extra cost; `onetime` runs one negative pass at the first step."),
                ParamSchema::float("delta", "Delta", 1.0, Some(0.0), Some(2.0)).help("Residual CFG's scale of the virtual residual noise."),
                ParamSchema::float("similarity", "Similarity Skip", 0.0, Some(0.0), Some(1.0)).help("Stochastic similarity filter threshold: when consecutive inputs are this similar the host may reuse its last result, saving GPU time on static scenes; 0 turns it off."),
                ParamSchema::path("lora", "LoRA").help("A LoRA weights file applied on top of the model."),
                ParamSchema::float("lora_weight", "LoRA Weight", 1.0, Some(0.0), Some(2.0)).help("Strength of the LoRA."),
            ]);
            inputs.push(img("control", true));
            capabilities.push(CapabilityRequirement {
                flag: "win+nvidia".parse().expect("valid flag"),
                fallback: Fallback::RouteRemote {
                    explanation: "Real-time diffusion runs locally on Windows with an NVIDIA GPU. Elsewhere the node routes to a LAN or cloud host on its route, or runs here at interactive rates.".into(),
                },
            });
            ("Img2img diffusion of the input, live (StreamDiffusion-class: stream batch, residual CFG, similarity filter, tiny VAE), with a blend of up to four prompts. Runs asynchronously on an AI Host; the Output Governor keeps the output at the project frame rate.".into(), 400)
        }
        Kind::Model => {
            params = common("local", 512);
            params.extend([
                ParamSchema::menu("adapter", "Adapter", &["builtin", "onnx", "diffusion.sd", "video.v2v"], "builtin").help("The host adapter that runs the model."),
                ParamSchema::string("model", "Model", "builtin.invert").help("Model id (a built-in such as `builtin.invert`, or one from the model manager) or a path."),
                ParamSchema::string("task", "Task", "").help("Adapter-specific task (for `onnx`: generic, depth, segment, pose, flow, background, upscale)."),
            ]);
            ("Runs any model an AI Host offers on the input, by adapter and model id or path, asynchronously; the Output Governor keeps the output at the project frame rate.".into(), 300)
        }
        Kind::Vision(task) => {
            params = common("local", 512);
            let (default_model, what) = match task {
                "depth" => (
                    "depth-anything-v2-small",
                    "Estimates depth: near is bright, far is dark.",
                ),
                "segment" => (
                    "",
                    "Segments people and objects: the mask is in alpha, classes are coloured.",
                ),
                "pose" => (
                    "dwpose",
                    "Finds body keypoints and draws the skeleton over black.",
                ),
                "flow" => (
                    "raft",
                    "Estimates optical flow between consecutive frames: red and green carry the horizontal and vertical motion around mid-grey.",
                ),
                "background" => (
                    "birefnet-lite",
                    "Removes the background: the input with the subject's mask in alpha.",
                ),
                "upscale" => (
                    "swin2sr-x4",
                    "Upscales the input with a super-resolution model.",
                ),
                _ => ("", "Runs an ONNX model of your choice on the input."),
            };
            params.push(
                ParamSchema::string("model", "Model", default_model)
                    .help("Model id from the model manager, or a path to an `.onnx` file."),
            );
            if task == "generic" {
                params.extend([
                    ParamSchema::string("input_name", "Input Name", "")
                        .help("The model's image input (empty: the first input)."),
                    ParamSchema::menu("layout", "Layout", &["nchw", "nhwc"], "nchw")
                        .help("Tensor layout the model expects."),
                    ParamSchema::vec("mean", "Mean", &[0.0, 0.0, 0.0])
                        .help("Subtracted from each RGB channel (0–1 scale) before inference."),
                    ParamSchema::vec("std", "Std", &[1.0, 1.0, 1.0])
                        .help("Each RGB channel is divided by this after the mean."),
                ]);
            }
            (
                format!(
                    "{what} Runs with ONNX Runtime on an AI Host, using the best execution provider there (Core ML, CUDA, TensorRT, DirectML or CPU); asynchronous, governed like every AI node."
                ),
                300,
            )
        }
        Kind::Video => {
            params = common("cloud:decart", 512);
            params.push(ParamSchema::string("model", "Model", "lucy-2.6").help(
                "Streaming video model (a cloud provider's model name, or a local model id).",
            ));
            params.extend(prompt_params());
            params.push(ParamSchema::int("seed", "Seed", 1, Some(0), None).help("Noise seed."));
            capabilities.push(CapabilityRequirement {
                flag: "win+nvidia".parse().expect("valid flag"),
                fallback: Fallback::RouteRemote {
                    explanation: "Streaming video models need an NVIDIA GPU with 24 GB or more locally; elsewhere the node runs on a LAN or cloud host on its route.".into(),
                },
            });
            ("Restyles live video with a streaming video-to-video model (FR-A11), on a local 24 GB+ NVIDIA host, a LAN host or a cloud provider; asynchronous, governed like every AI node.".into(), 300)
        }
    };
    OperatorSchema {
        type_id: type_id.into(),
        family: OpFamily::Ai,
        label: label.into(),
        summary,
        version: 1,
        params,
        inputs,
        outputs: vec![img("out", false)],
        capabilities,
        cook_budget_us: budget,
        time_dependent: true,
        sink: false,
    }
}
