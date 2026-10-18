# Taper (`pnt.taper`)

PNT · version 1 · cook budget 60 µs

Scales `P` across an axis by an amount that changes along it: Start at the origin to End at one Length along the axis (held beyond).

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
| `origin` | Origin | vec3 | `[0.0,-1.0,0.0]` |  | Where the taper starts. |
| `axis` | Axis | vec3 | `[0.0,1.0,0.0]` |  | Direction of the taper. |
| `start` | Start | float | `1.0` |  | Scale across the axis at the origin. |
| `end` | End | float | `0.0` |  | Scale across the axis at one Length along it. |
| `length` | Length | float | `2.0` | ≥ 0.000001 | Distance over which the scale goes from Start to End. |

## Platforms

All platforms.

## Examples

- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `taper`
