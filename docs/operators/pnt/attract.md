# Attract (`pnt.attract`)

PNT · version 1 · cook budget 60 µs

A force toward a point (or away from it with a negative strength) on the velocity `v` (created at zero when missing), reading `P`: a spring, a constant pull or an inverse-square pull, optionally fading out with distance.

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
| `point` | Point | vec3 | `[0.0,0.0,0.0]` |  | The point particles are pulled toward. |
| `strength` | Strength | float | `1.0` |  | Acceleration scale (negative repels). |
| `mode` | Mode | menu | `"spring"` | `spring`, `constant`, `inverse_square` | spring: grows with distance; constant: the same at any distance; inverse_square: strongest close in (softened by Softening). |
| `radius` | Radius | float | `0.0` | ≥ 0 | Distance at which the effect fades out smoothly (0: no falloff, everywhere at full strength). |
| `softening` | Softening | float | `0.01` | ≥ 0.000001 | inverse_square: the squared distance below which the force stops growing. |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `home`
- [PNT Solver](../../../examples/pnt-solver.atidarla/) — `pull`
