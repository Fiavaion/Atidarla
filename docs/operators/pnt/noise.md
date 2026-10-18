# Noise (`pnt.noise`)

PNT · version 1 · cook budget 120 µs

Curl noise, a swirling divergence-free field: displaces `P`, or adds a force to the velocity `v` (created at zero when missing) so particles flow. Animate it through `evolution` (for example the expression `time * 0.3`).

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
| `mode` | Mode | menu | `"displace"` | `displace`, `velocity` | displace: P += amplitude × curl(P); velocity: v += amplitude × curl(P) × the frame's time step. |
| `amplitude` | Amplitude | float | `0.2` |  | Strength: a distance (displace) or an acceleration in units per second² (velocity). |
| `frequency` | Frequency | float | `2.0` | ≥ 0 | Spatial frequency: noise cells per unit. |
| `offset` | Offset | vec3 | `[0.0,0.0,0.0]` |  | Moves the noise field (in noise cells). |
| `evolution` | Evolution | float | `0.0` |  | Moves the field along a diagonal through noise space, so the swirls drift and change; drive it from time to animate. |
| `seed` | Seed | int | `0` | ≥ 0 | Selects a different field. |

## Platforms

All platforms.

## Examples

- [Particles](../../../examples/particles.atidarla/) — `flow`
- [Points](../../../examples/points.atidarla/) — `swirl`, `ripple`
