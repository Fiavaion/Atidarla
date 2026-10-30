# Video Model (`ai.video_model`)

AI · version 1 · cook budget 300 µs

Restyles live video with a streaming video-to-video model (FR-A11), on a local 24 GB+ NVIDIA host, a LAN host or a cloud provider; asynchronous, governed like every AI node.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `image` | Img |  |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `route` | Route | string | `"cloud:decart"` |  | Where the node runs, in order of preference: `local`, `lan:<name>` and `cloud:<name>` separated by `→`, `->` or commas. The first host that is ready and can run the model wins; a host that meets the node's real-time need is preferred. Cloud hosts are skipped in Sovereign mode and once the project's spend cap is reached. |
| `width` | Width | int | `512` | 8 – 2048 | Width of the frames sent to the model; the result is scaled to the output. |
| `height` | Height | int | `512` | 8 – 2048 | Height of the frames sent to the model. |
| `policy` | Governor | menu | `"blend"` | `hold`, `blend`, `flow`, `passthrough` | How the output fills the frames between AI results, at the project frame rate: `hold` shows the newest result; `blend` cross-fades from the previous result to the newest over one AI interval; `flow` interpolates along the motion between them; `passthrough` shows the input with the effect below whenever no result is fresh. |
| `effect` | Effect | float | `0.5` | 0 – 1 | Strength of the desaturate-and-tint effect on the input shown before the first result (and when stale under `passthrough`). |
| `in_flight` | In Flight | int | `1` | 1 – 4 | Frames sent to the host at once. More hides network latency; fewer keeps the result closer to the input. |
| `budget_ms` | Latency Budget | float | `0.0` | 0 – 5000 | When the round trip stays above this many milliseconds for 3 s, the node moves to the next host on its route; 0 turns it off. |
| `model` | Model | string | `"lucy-2.6"` |  | Streaming video model (a cloud provider's model name, or a local model id). |
| `prompts_from` | Prompts From | string | `""` |  | The name of a Stage deck's Mixer: the Prompt Clips playing on its crossfader sides A and B become prompts 1 and 2, blended by the crossfader, with the leading clip's seed, negative and LoRA (AI clips). Empty: the prompts below. |
| `prompt1` | Prompt 1 | string | `"a watercolour painting"` |  | A prompt of the Prompt Blend: the model sees the weighted mix of the prompts' embeddings. |
| `weight1` | Weight 1 | float | `1.0` | 0 – 1 | This prompt's share of the blend (weights are normalised). Bind it to a channel or the crossfader to morph between prompts. |
| `prompt2` | Prompt 2 | string | `""` |  | A prompt of the Prompt Blend: the model sees the weighted mix of the prompts' embeddings. |
| `weight2` | Weight 2 | float | `0.0` | 0 – 1 | This prompt's share of the blend (weights are normalised). Bind it to a channel or the crossfader to morph between prompts. |
| `prompt3` | Prompt 3 | string | `""` |  | A prompt of the Prompt Blend: the model sees the weighted mix of the prompts' embeddings. |
| `weight3` | Weight 3 | float | `0.0` | 0 – 1 | This prompt's share of the blend (weights are normalised). Bind it to a channel or the crossfader to morph between prompts. |
| `prompt4` | Prompt 4 | string | `""` |  | A prompt of the Prompt Blend: the model sees the weighted mix of the prompts' embeddings. |
| `weight4` | Weight 4 | float | `0.0` | 0 – 1 | This prompt's share of the blend (weights are normalised). Bind it to a channel or the crossfader to morph between prompts. |
| `seed` | Seed | int | `1` | ≥ 0 | Noise seed. |

## Platforms

- Requires `win+nvidia`; elsewhere: Streaming video models need an NVIDIA GPU with 24 GB or more locally; elsewhere the node runs on a LAN or cloud host on its route.

## Examples

- [Live AI](../../../examples/ai-live.atidarla/) — `video1`
