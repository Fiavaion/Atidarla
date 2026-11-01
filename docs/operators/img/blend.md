# Blend (`img.blend`)

IMG · version 1 · cook budget 60 µs

Crossfades from input A to input B.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `a` | Img | optional |
| `b` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `mix` | Mix | float | `0.5` | 0 – 1 | 0 shows A, 1 shows B. |

## Platforms

All platforms.

## Examples

- [Keying](../../../examples/keying.lasair/) — `mix`
- [Time effects](../../../examples/time-effects.lasair/) — `echo`
