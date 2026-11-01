# Switch (`img.switch`)

IMG · version 1 · cook budget 60 µs

Outputs one of up to four inputs, optionally blending between neighbours.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in0` | Img | optional |
| `in1` | Img | optional |
| `in2` | Img | optional |
| `in3` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `index` | Index | float | `0.0` | 0 – 3 | Which input to show; fractional values blend when Blend is on. |
| `blend` | Blend | toggle | `false` |  | Crossfade between neighbouring inputs for fractional indices. |

## Platforms

All platforms.

## Examples

- [Beat switcher](../../../examples/beat-switcher.lasair/) — `switch1`
