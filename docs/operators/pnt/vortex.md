# Vortex (`pnt.vortex`)

PNT · version 1 · cook budget 60 µs

A swirl about an axis on the velocity `v` (reading `P`): spins points around the axis, optionally pulls them in toward it and lifts them along it, fading out with distance from the axis.

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
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | A point on the axis. |
| `axis` | Axis | vec3 | `[0.0,0.0,1.0]` |  | Direction of the axis (spin is counter-clockwise looking down it). |
| `strength` | Strength | float | `1.0` |  | Tangential acceleration (negative spins the other way). |
| `pull` | Pull | float | `0.0` |  | Acceleration toward the axis (negative pushes out). |
| `lift` | Lift | float | `0.0` |  | Acceleration along the axis. |
| `radius` | Radius | float | `0.0` | ≥ 0 | Distance at which the effect fades out smoothly (0: no falloff, everywhere at full strength). |

## Platforms

All platforms.

## Examples

- [PNT Fountain](../../../examples/pnt-fountain.atidarla/) — `spin`
