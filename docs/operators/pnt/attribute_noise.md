# Attribute Noise (`pnt.attribute_noise`)

PNT · version 1 · cook budget 80 µs

Fractal noise on any point attribute: gradient noise (a different field per component) or curl noise, evaluated at `P` (or another vector attribute), scaled, centred and combined with the attribute's value (set, add, multiply, max, min). Animate it with Evolution.

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
| `name` | Name | string | `"Cd"` |  | The attribute changed (created at zero, with Type, when missing). |
| `type` | Type | menu | `"vec4"` | `float`, `vec2`, `vec3`, `vec4`, `int` | The type of a new attribute (an existing one keeps its type). |
| `noise` | Noise | menu | `"gradient"` | `gradient`, `curl` | gradient: smooth fractal noise, a different field per component; curl: a divergence-free swirl (three components; the fourth gets gradient noise). |
| `position` | Position | string | `"P"` |  | The attribute the noise is evaluated at (any vector, usually `P`; `uv` for a texture-like pattern). |
| `amplitude` | Amplitude | float | `0.5` |  | Scale of the noise (about −1 to 1 before scaling). |
| `centre` | Centre | float | `0.0` |  | Added to the noise: 0.5 with amplitude 0.5 gives values from 0 to 1. |
| `frequency` | Frequency | float | `2.0` | ≥ 0 | Noise cells per unit. |
| `octaves` | Octaves | int | `3` | 1 – 8 | Layers of detail, each at twice the frequency. |
| `roughness` | Roughness | float | `0.5` | 0 – 1 | Amplitude of each octave relative to the one before. |
| `offset` | Offset | vec3 | `[0.0,0.0,0.0]` |  | Moves the noise field. |
| `evolution` | Evolution | float | `0.0` |  | Moves the field through a fourth direction so the pattern changes rather than slides; drive it from time. |
| `seed` | Seed | int | `0` | ≥ 0 | Selects a different field. |
| `operation` | Operation | menu | `"add"` | `set`, `add`, `multiply`, `max`, `min` | How the result meets the attribute's value: set (replace), add, multiply, max or min. |
| `amount` | Amount | float | `1.0` |  | Blends from the attribute's value (0) to the combined result (1). |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.lasair/) — `grain`
