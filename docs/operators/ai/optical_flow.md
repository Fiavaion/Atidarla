# Optical Flow (`ai.optical_flow`)

AI · version 1 · cook budget 300 µs

Estimates optical flow between consecutive frames: red and green carry the horizontal and vertical motion around mid-grey. Runs with ONNX Runtime on an AI Host, using the best execution provider there (Core ML, CUDA, TensorRT, DirectML or CPU); asynchronous, governed like every AI node.

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
| `route` | Route | string | `"local"` |  | Where the node runs, in order of preference: `local`, `lan:<name>` and `cloud:<name>` separated by `→`, `->` or commas. The first host that is ready and can run the model wins; a host that meets the node's real-time need is preferred. Cloud hosts are skipped in Sovereign mode and once the project's spend cap is reached. |
| `width` | Width | int | `512` | 8 – 2048 | Width of the frames sent to the model; the result is scaled to the output. |
| `height` | Height | int | `512` | 8 – 2048 | Height of the frames sent to the model. |
| `policy` | Governor | menu | `"blend"` | `hold`, `blend`, `flow`, `passthrough` | How the output fills the frames between AI results, at the project frame rate: `hold` shows the newest result; `blend` cross-fades from the previous result to the newest over one AI interval; `flow` interpolates along the motion between them; `passthrough` shows the input with the effect below whenever no result is fresh. |
| `effect` | Effect | float | `0.5` | 0 – 1 | Strength of the desaturate-and-tint effect on the input shown before the first result (and when stale under `passthrough`). |
| `in_flight` | In Flight | int | `1` | 1 – 4 | Frames sent to the host at once. More hides network latency; fewer keeps the result closer to the input. |
| `budget_ms` | Latency Budget | float | `0.0` | 0 – 5000 | When the round trip stays above this many milliseconds for 3 s, the node moves to the next host on its route; 0 turns it off. |
| `model` | Model | string | `"raft"` |  | Model id from the model manager, or a path to an `.onnx` file. |

## Platforms

All platforms.

## Examples

- [Live AI](../../../examples/ai-live.atidarla/) — `flow1`
