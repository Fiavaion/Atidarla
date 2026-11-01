# Projection (`img.projection`)

IMG · version 1 · cook budget 120 µs

Converts between panoramic and camera projections: equirectangular (360°), fisheye, perspective and a cubemap cross, with a yaw/pitch/roll rotation.

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
| `from` | From | menu | `"equirect"` | `equirect`, `fisheye`, `perspective`, `cube` | Projection of the input image. |
| `to` | To | menu | `"perspective"` | `equirect`, `fisheye`, `perspective`, `cube` | Projection of the output image. |
| `fov` | Field of View | float | `90.0` | 1 – 360 | Degrees: horizontal for `perspective`, full circle for `fisheye` (applies to whichever side uses them). |
| `rotate` | Rotate | vec3 | `[0.0,0.0,0.0]` |  | Yaw, pitch and roll in degrees, applied to the view. |

## Platforms

All platforms.

## Examples

- [Shaders](../../../examples/shaders.lasair/) — `fisheye`
