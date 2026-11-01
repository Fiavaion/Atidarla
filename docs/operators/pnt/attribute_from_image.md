# Attribute from Image (`pnt.attribute_from_image`)

PNT · version 1 · cook budget 80 µs

Looks an image up at each point and writes the colour, or a measure of it, into a point attribute of any type (vec4 gets the colour, vec3 its RGB, vec2 its red and green, float and int the measure): looked up by a uv attribute, by `P` projected on a plane through a rectangle, or by point number (row by row from the top-left, for data images). Set, add or multiply, blended by Amount.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `image` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name` | Name | string | `"Cd"` |  | The attribute written (created at zero when missing). |
| `type` | Type | menu | `"vec4"` | `float`, `vec2`, `vec3`, `vec4`, `int` | Its type; an attribute of another type with the same name is replaced. |
| `measure` | Measure | menu | `"luminance"` | `luminance`, `red`, `green`, `blue`, `alpha` | float, int: the pixel's luminance or one channel. |
| `lookup` | Lookup | menu | `"uv"` | `uv`, `planar`, `index` | uv: the attribute UV Attribute (0-1, v up); planar: P projected on Plane, mapped through the rectangle Centre ± Size / 2 to the whole image; index: point i reads pixel i, row by row from the top-left. |
| `uv` | UV Attribute | string | `"uv"` |  | uv: the vec2 (or vec3, vec4: its first two components) attribute read. |
| `plane` | Plane | menu | `"xy"` | `xy`, `xz`, `zy` | planar: the plane P is projected on (its first axis is the image's horizontal). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | planar: the centre of the rectangle the image covers. |
| `size` | Size | vec2 | `[2.0,2.0]` |  | planar: the rectangle's size along the plane's two axes. |
| `bilinear` | Bilinear | toggle | `true` |  | Interpolates between pixels; off takes the nearest pixel. |
| `repeat` | Repeat | toggle | `false` |  | Repeats the image outside 0-1; off holds its edge pixels. |
| `operation` | Operation | menu | `"set"` | `set`, `add`, `multiply` | set: the looked-up value; add, multiply: combined with the attribute's value. |
| `amount` | Amount | float | `1.0` |  | Blends from the attribute's value (0) to the result (1). |

## Platforms

All platforms.

## Examples

- [PNT Capture](../../../examples/pnt-capture.lasair/) — `tint`
