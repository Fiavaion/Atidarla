# Wind (`pnt.wind`)

PNT · version 1 · cook budget 80 µs

Air moving at a velocity, with gusts from curl noise: the velocity `v` relaxes toward the wind's at the response rate (reading `P` for the gusts).

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
| `velocity` | Velocity | vec3 | `[1.0,0.0,0.0]` |  | The wind's velocity (units per second). |
| `response` | Response | float | `1.0` | ≥ 0 | How fast points pick up the wind's velocity (per second). |
| `gust` | Gust | float | `0.5` |  | Strength of the gusts (units per second). |
| `gust_frequency` | Gust Frequency | float | `1.0` | ≥ 0 | Noise cells per unit of the gust field. |
| `evolution` | Evolution | float | `0.0` |  | Moves the gust field through noise space; drive it from time (for example `time * 0.5`) to animate. |
| `seed` | Seed | int | `0` | ≥ 0 | Selects a different noise field. |

## Platforms

All platforms.

## Examples

No example project uses it yet.
