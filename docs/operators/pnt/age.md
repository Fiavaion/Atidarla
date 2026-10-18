# Age (`pnt.age`)

PNT · version 1 · cook budget 200 µs

Ages points and removes them by rules: `age += dt` each frame (created at zero when missing), then removes points that reached their `life` (or the Life parameter when they have none), points inside or outside a region, and points slower than a speed. For simulation loops (Solver, or a feedback chain); Particles ages and removes its own particles.

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
| `step` | Step | toggle | `true` |  | Adds the frame's time step to `age`. |
| `kill_dead` | Kill Dead | toggle | `true` |  | Removes points whose `age` reached their life. |
| `life` | Life | float | `2.0` | ≥ 0 | The life, in seconds, of points without a `life` attribute. |
| `region` | Region | menu | `"none"` | `none`, `inside_box`, `outside_box`, `inside_sphere`, `outside_sphere` | Also removes points inside or outside a box or a sphere. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | The region's centre. |
| `size` | Size | vec3 | `[4.0,4.0,4.0]` |  | The box's extent along X, Y and Z. |
| `radius` | Radius | float | `2.0` | ≥ 0 | The sphere's radius. |
| `min_speed` | Min Speed | float | `0.0` | ≥ 0 | Also removes points slower than this (units per second; 0: off; needs `v`). |

## Platforms

All platforms.

## Examples

No example project uses it yet.
