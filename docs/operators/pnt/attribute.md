# Attribute (`pnt.attribute`)

PNT · version 1 · cook budget 60 µs

Creates or sets a point attribute of any type: a constant, a ramp between two values along an axis, a ramp over the point numbers, or gradient noise at `P`. An attribute of another type with the same name is replaced.

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
| `name` | Name | string | `"pscale"` |  | The attribute written. |
| `type` | Type | menu | `"float"` | `float`, `vec2`, `vec3`, `vec4`, `int` | Its type (int is a 32-bit signed integer; values are rounded). |
| `source` | Source | menu | `"constant"` | `constant`, `ramp`, `index`, `noise` | constant: Value; ramp: Value at Start to Value 2 at End, along the line between them; index: Value for the first point to Value 2 for the last; noise: Value + Amplitude × gradient noise at P (a different noise per component). |
| `value` | Value | vec4 | `[1.0,1.0,1.0,1.0]` |  | The value (components past the type's are ignored). |
| `value2` | Value 2 | vec4 | `[0.0,0.0,0.0,0.0]` |  | ramp, index: the value at the other end. |
| `start` | Start | vec3 | `[-1.0,0.0,0.0]` |  | ramp: where the ramp gives Value. |
| `end` | End | vec3 | `[1.0,0.0,0.0]` |  | ramp: where it gives Value 2 (clamped beyond both ends). |
| `amplitude` | Amplitude | float | `1.0` |  | noise: scale of the noise. |
| `frequency` | Frequency | float | `1.0` | ≥ 0 | noise: noise cells per unit. |
| `offset` | Offset | vec3 | `[0.0,0.0,0.0]` |  | noise: moves the noise field (animate it to make the values flow). |
| `seed` | Seed | int | `0` | ≥ 0 | noise: selects a different field. |

## Platforms

All platforms.

## Examples

- [PNT Solver](../../../examples/pnt-solver.atidarla/) — `kick`
