# Gravity (`pnt.gravity`)

PNT · version 1 · cook budget 40 µs

A constant acceleration on the velocity `v` (created at zero when missing).

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
| `acceleration` | Acceleration | vec3 | `[0.0,-9.8,0.0]` |  | Units per second² (Earth's gravity in metres: [0, -9.8, 0]). |

## Platforms

All platforms.

## Examples

- [PNT Fountain](../../../examples/pnt-fountain.lasair/) — `gravity`
