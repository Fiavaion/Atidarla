# Turbulence (`pnt.turbulence`)

PNT · version 1 · cook budget 150 µs

Fractal curl noise (several octaves of the curl noise `pnt.noise` uses) as a force on the velocity `v`: swirls at several scales, divergence-free, so points mix without bunching up.

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
| `amplitude` | Amplitude | float | `1.0` |  | Acceleration of the first octave (units per second²); each further octave has half. |
| `frequency` | Frequency | float | `1.0` | ≥ 0 | Noise cells per unit of the first octave; each further octave doubles it. |
| `octaves` | Octaves | int | `3` | 1 – 8 | Number of octaves summed. |
| `offset` | Offset | vec3 | `[0.0,0.0,0.0]` |  | Moves the field (in noise cells). |
| `evolution` | Evolution | float | `0.0` |  | Moves the field through noise space; drive it from time to animate. |
| `seed` | Seed | int | `0` | ≥ 0 | Selects a different noise field. |

## Platforms

All platforms.

## Examples

- [PNT Fountain](../../../examples/pnt-fountain.lasair/) — `stir`
- [PNT Solver](../../../examples/pnt-solver.lasair/) — `churn`
