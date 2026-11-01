# UV Unwrap (`msh.uv_unwrap`)

MSH · version 1 · cook budget 100 µs

Cuts the surface into charts by normal angle, flattens each (least squares conformal maps, or planar) and packs them into the unit square: writes `uv` (points split along seams) and the primitive attribute `chart`.

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
| `method` | Method | menu | `"conformal"` | `conformal`, `planar` | conformal: each chart flattened keeping angles (least squares conformal maps); planar: each chart projected onto its first polygon's plane. |
| `angle` | Chart Angle | float | `60.0` | 1 – 89 | Degrees a polygon's normal may differ from its chart's first polygon; smaller makes more, flatter charts. |
| `margin` | Margin | float | `0.01` | 0 – 0.25 | Space around each chart, as a fraction of the square's side. |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.lasair/) — `charts`
