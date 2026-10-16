# Crop (`img.crop`)

IMG · version 1 · cook budget 60 µs

Keeps a rectangle of the input. By default the output is that rectangle at its own pixel size; `input` size stretches it to the input's size instead.

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
| `min` | Min | vec2 | `[0.25,0.25]` |  | Bottom-left corner of the kept area, as a fraction of the image. |
| `max` | Max | vec2 | `[0.75,0.75]` |  | Top-right corner of the kept area. |
| `size` | Size | menu | `"crop"` | `crop`, `input` | `crop` outputs the kept pixels at their size; `input` keeps the input size. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
