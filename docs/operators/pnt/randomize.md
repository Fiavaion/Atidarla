# Randomize (`pnt.randomize`)

PNT · version 1 · cook budget 60 µs

Sets a point attribute to seeded random values: uniform between Min and Max per component, normal (Gaussian, mean Min, deviation Max), or random unit directions scaled by Max. A function of the seed and each point's `id` (or number), so the same points get the same values on every run and frame.

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
| `name` | Name | string | `"Cd"` |  | The attribute written. |
| `type` | Type | menu | `"vec4"` | `float`, `vec2`, `vec3`, `vec4`, `int` | Its type. |
| `distribution` | Distribution | menu | `"uniform"` | `uniform`, `normal`, `direction` | uniform: Min to Max; normal: mean Min, standard deviation Max; direction: unit vectors (the type's components) times Max. |
| `min` | Min | vec4 | `[0.0,0.0,0.0,1.0]` |  | uniform: lowest values; normal: means. |
| `max` | Max | vec4 | `[1.0,1.0,1.0,1.0]` |  | uniform: highest values; normal: deviations; direction: lengths. |
| `seed` | Seed | int | `0` | ≥ 0 | Selects different values. |
| `by_id` | By ID | toggle | `true` |  | Seeds each point from its `id` attribute when it has one (values follow points that move in the list, such as particles), else from its number. |

## Platforms

All platforms.

## Examples

No example project uses it yet.
