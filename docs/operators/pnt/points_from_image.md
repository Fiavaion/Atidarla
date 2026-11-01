# Points from Image (`pnt.points_from_image`)

PNT · version 1 · cook budget 120 µs

A grid of points over an image, one per cell of Columns × rows, coloured by the image: writes `P` (in the XY plane, Width wide, centred on Centre, optionally pushed along Z by a measure of the pixel), `Cd` (the pixel's colour), `uv` (0-1, v up), `value` (the measure: luminance or a channel) and `id`. The count follows the image's size; with no image there are no points. Remove dark points with Delete on `value`.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `image` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `columns` | Columns | int | `128` | 1 – 4096 | Points across the image (at most one per pixel); the rows follow the image's aspect ratio, so the count is Columns × rows. Each point samples the image at its cell's centre. |
| `width` | Width | float | `2.0` | ≥ 0 | The grid's width; its height follows the image's aspect ratio. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | The grid's centre. |
| `measure` | Measure | menu | `"luminance"` | `luminance`, `red`, `green`, `blue`, `alpha` | What `value` holds: the pixel's Rec. 709 luminance or one channel. |
| `displace` | Displace | float | `0.0` |  | Moves each point along +Z by Displace × value (a relief from the image). |
| `bilinear` | Bilinear | toggle | `true` |  | Interpolates between pixels; off takes the nearest pixel. |

## Platforms

All platforms.

## Examples

- [PNT Capture](../../../examples/pnt-capture.lasair/) — `relief`
