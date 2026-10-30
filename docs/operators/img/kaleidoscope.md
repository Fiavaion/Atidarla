# Kaleidoscope (`img.kaleidoscope`)

IMG · version 1 · cook budget 80 µs

Folds the input into mirrored segments around the centre.

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
| `segments` | Segments | int | `6` | 1 – 64 | Number of mirrored wedges. |
| `rotation` | Rotation | float | `0.0` |  | Rotation of the pattern in degrees. |

## Platforms

All platforms.

## Examples

- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.atidarla/) — `kaleidoscope1`
- [Timeline show](../../../examples/timeline-show.atidarla/) — `kaleido`
