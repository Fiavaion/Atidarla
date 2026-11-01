# Sweep (`msh.sweep`)

MSH · version 1 · cook budget 100 µs

Sweeps a profile along a curve: the first primitive of `profile` (drawn in the XY plane) or a circle, along the first primitive of `curve`, with rotation-minimising frames, scale and twist. Writes `N` and `uv`.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `curve` | Geo | optional |
| `profile` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `radius` | Radius | float | `0.1` | ≥ 0 | Without a profile input: the radius of the circle swept. |
| `divisions` | Divisions | int | `12` | 3 – 4096 | Without a profile input: points around the circle. |
| `scale_end` | End Scale | float | `1.0` | ≥ 0 | The profile's scale at the end of the curve (it grows or shrinks linearly from 1). |
| `twist` | Twist | float | `0.0` |  | Degrees the profile turns about the curve from start to end. |
| `caps` | Caps | toggle | `true` |  | Closes both ends of an open curve when the profile is closed. |

## Platforms

All platforms.

## Examples

- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.lasair/) — `frame`
