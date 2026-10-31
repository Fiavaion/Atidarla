# Transform (`pnt.transform`)

PNT · version 1 · cook budget 60 µs

Translates, rotates and scales points about a pivot: moves `P`, and turns `v` (rotation and scale) and `N` (kept perpendicular to surfaces, unit length) when they exist.

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
| `translate` | Translate | vec3 | `[0.0,0.0,0.0]` |  | Moves the points. |
| `rotate` | Rotate | vec3 | `[0.0,0.0,0.0]` |  | Rotation about X, then Y, then Z, in degrees, about the pivot. |
| `scale` | Scale | vec3 | `[1.0,1.0,1.0]` |  | Scale along X, Y and Z about the pivot. |
| `uniform_scale` | Uniform Scale | float | `1.0` |  | Multiplies the scale on every axis. |
| `pivot` | Pivot | vec3 | `[0.0,0.0,0.0]` |  | The point rotation and scale are about. |

## Platforms

All platforms.

## Examples

- [PNT Capture](../../../examples/pnt-capture.atidarla/) — `dome_place`, `relief_place`, `spiral_place`, `sensor_place`, `lattice_place`, `pulse_place`, `joints_place`
- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `left`, `turn`
