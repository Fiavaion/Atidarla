# Twist (`pnt.twist`)

PNT · version 1 · cook budget 60 µs

Twists `P` about an axis: points turn by an angle that grows along the axis, Angle degrees per Length.

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
| `origin` | Origin | vec3 | `[0.0,0.0,0.0]` |  | A point on the axis (no rotation there). |
| `axis` | Axis | vec3 | `[0.0,1.0,0.0]` |  | Direction of the axis. |
| `angle` | Angle | float | `90.0` |  | Rotation in degrees at one Length from the origin. |
| `length` | Length | float | `1.0` | ≥ 0.000001 | Distance along the axis over which the rotation reaches Angle. |

## Platforms

All platforms.

## Examples

- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `twist`
