# Control Prep (`ai.control_prep`)

AI · version 1 · cook budget 150 µs

Prepares a control image for diffusion conditioning, on the GPU: Canny-style edges (thin, with hysteresis), soft edges, line art (dark lines on white), scribble (thick lines) or a threshold. Feed it to the Diffusion node's `control` input.

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
| `mode` | Mode | menu | `"canny"` | `canny`, `soft_edge`, `lineart`, `scribble`, `threshold` | `canny`: thin edges kept where strong, or weak and touching a strong one; `soft_edge`: edge strength; `lineart`: soft edges as dark lines on white; `scribble`: thick white strokes; `threshold`: white where brighter than High. |
| `low` | Low | float | `0.1` | 0 – 1 | Canny: weaker edges are dropped; edges between Low and High survive only next to a strong edge. |
| `high` | High | float | `0.25` | 0 – 1 | Canny: edges at least this strong are always kept. Threshold: the luminance cut. |
| `invert` | Invert | toggle | `false` |  | Swap black and white. |

## Platforms

All platforms.

## Examples

- [Live AI](../../../examples/ai-live.lasair/) — `edges`
