# Channel Mix (`img.channel_mix`)

IMG · version 1 · cook budget 60 µs

Rebuilds each output channel as a weighted sum of the input's R, G, B and A.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `red` | Red | vec4 | `[1.0,0.0,0.0,0.0]` |  | Weights of input R, G, B, A in the output red. |
| `green` | Green | vec4 | `[0.0,1.0,0.0,0.0]` |  | Weights for the output green. |
| `blue` | Blue | vec4 | `[0.0,0.0,1.0,0.0]` |  | Weights for the output blue. |
| `alpha` | Alpha | vec4 | `[0.0,0.0,0.0,1.0]` |  | Weights for the output alpha. |

## Platforms

All platforms.

## Examples

- [Image processing](../../../examples/image-processing.lasair/) — `swapped`
