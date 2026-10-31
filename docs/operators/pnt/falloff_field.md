# Falloff Field (`pnt.falloff_field`)

PNT · version 1 · cook budget 40 µs

A falloff weight per point from a shape, as Notch-style effectors use: a sphere, a box, a half-space or a cylinder, with a smooth, linear or hard edge, written into a float attribute (`falloff`) and combined with what is there (set, add, multiply, max, min), so several fields layer. Read it with Attribute Math (mix by the weight), Colour by Attribute, Delete or a Kernel.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name` | Name | string | `"falloff"` |  | The float attribute written (a weight, 1 inside the shape to 0 outside). |
| `shape` | Shape | menu | `"sphere"` | `sphere`, `box`, `plane`, `cylinder` | sphere: around Centre within Radius; box: inside Size around Centre, fading over Radius outside it; plane: on the Direction side of the plane through Centre, fading over Radius; cylinder: around the axis through Centre along Direction, within Radius. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | The shape's centre (a point on the plane or the axis). |
| `radius` | Radius | float | `1.0` | ≥ 0 | sphere, cylinder: the radius; box, plane: the width of the fade. |
| `size` | Size | vec3 | `[1.0,1.0,1.0]` |  | box: the full weight region's extent along X, Y and Z. |
| `direction` | Direction | vec3 | `[0.0,1.0,0.0]` |  | plane: its normal (the weight is 1 on this side); cylinder: its axis. |
| `inner` | Inner | float | `0.0` | 0 – 1 | sphere, cylinder: the fraction of Radius at full weight before the fade starts. |
| `curve` | Curve | menu | `"smooth"` | `smooth`, `linear`, `step` | The fade: smooth (smoothstep), linear, or step (no fade: 1 inside, 0 outside). |
| `invert` | Invert | toggle | `false` |  | 1 − the weight: full outside the shape. |
| `operation` | Operation | menu | `"set"` | `set`, `add`, `multiply`, `max`, `min` | How the result meets the attribute's value: set (replace), add, multiply, max or min. |
| `amount` | Amount | float | `1.0` |  | Blends from the attribute's value (0) to the combined result (1). |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `spot`, `band`
