# Emitter (`pnt.emitter`)

PNT · version 1 · cook budget 150 µs

Emits particles from the points of `source` into the particle state on `in` (for example a Particles system with rate 0, in its feedback loop): Rate per second from random source points, with a velocity, a speed along the source's normal `N`, a share of its velocity `v` and a random spread. Writes `P`, `v`, `age`, `life`, `id` (and `Cd` when the state has it) of the new particles; the state's capacity limits the count.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `source` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `rate` | Rate | float | `10000.0` | ≥ 0 | Particles emitted per second. |
| `life` | Life | float | `3.0` | ≥ 0.001 | Seconds a new particle lives. |
| `life_variance` | Life Variance | float | `0.25` | 0 – 1 | Random variation of the life, as a fraction. |
| `velocity` | Velocity | vec3 | `[0.0,0.0,0.0]` |  | Initial velocity added to every particle. |
| `normal_speed` | Normal Speed | float | `1.0` |  | Speed along the source point's `N` (when it has one). |
| `inherit` | Inherit | float | `0.0` |  | Share of the source point's `v` the particle starts with. |
| `spread` | Spread | float | `0.2` | ≥ 0 | Random speed added in any direction, up to this. |
| `jitter` | Jitter | float | `0.0` | ≥ 0 | Random offset from the source point, up to this distance. |
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | `Cd` of a new particle, when the state has `Cd`. |
| `id_offset` | ID Offset | int | `1073741824` | ≥ 0 | Added to the emission number to make `id`, so these particles' ids differ from another emitter's. |
| `seed` | Seed | int | `1` | ≥ 0 | Random seed. |
| `reset` | Reset | toggle | `false` |  | While on, the emission count restarts (ids and random choices start over). |

## Platforms

All platforms.

## Examples

- [PNT Fountain](../../../examples/pnt-fountain.atidarla/) — `nozzle`
