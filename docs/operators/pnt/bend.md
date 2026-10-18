# Bend (`pnt.bend`)

PNT · version 1 · cook budget 60 µs

Bends `P` around an arc: the stretch of Length along Axis from Origin curves toward Direction through Angle degrees; beyond it points continue straight along the bent end.

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
| `origin` | Origin | vec3 | `[0.0,-1.0,0.0]` |  | Where the bend starts. |
| `axis` | Axis | vec3 | `[0.0,1.0,0.0]` |  | Direction along which the bend runs. |
| `direction` | Direction | vec3 | `[1.0,0.0,0.0]` |  | Direction the shape bends toward (made perpendicular to the axis). |
| `angle` | Angle | float | `90.0` |  | Total bend over the length, in degrees. |
| `length` | Length | float | `2.0` | ≥ 0.000001 | Length of the bent stretch. |

## Platforms

All platforms.

## Examples

- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `bend`
