# Time Machine (`img.time_machine`)

IMG · version 1 · cook budget 200 µs

Shows each pixel of `in` from a different moment: the luminance of `time` (black = now, white = oldest) picks how many frames back, blending between frames. Without `time`, the whole image is delayed by Offset.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |
| `time` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `frames` | Frames | int | `30` | 2 – 120 | How many past frames are kept (memory grows with this and the resolution). |
| `offset` | Offset | float | `0.0` | 0 – 1 | Added to the time map: a fraction of the history. |
| `scale` | Scale | float | `1.0` |  | Multiplies the time map's luminance. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
