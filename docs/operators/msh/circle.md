# Circle (`msh.circle`)

MSH · version 1 · cook budget 100 µs

A circle or arc: one polygon, an outline (polyline) or points, in a chosen plane.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `type` | Type | menu | `"polygon"` | `polygon`, `polyline`, `points` | polygon: one filled polygon; polyline: a closed outline (its last point is its first); points: points only. |
| `plane` | Plane | menu | `"xy"` | `xy`, `xz`, `yz` | The plane the shape lies in (its normal is +Z for xy, +Y for xz, +X for yz). |
| `radius` | Radius | float | `0.5` | ≥ 0 | Radius. |
| `divisions` | Divisions | int | `32` | 3 – 1000000 | Points around. |
| `arc` | Arc | float | `360.0` | 0 – 360 | Degrees covered; below 360 the outline is open (polygon: a pie slice through the centre). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.atidarla/) — `ring0`, `ring1`, `ring2`, `ring3`
- [Mesh Primitives](../../../examples/mesh-primitives.atidarla/) — `outline`
- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.atidarla/) — `path`, `square`
