# Measure (`msh.measure`)

MSH · version 1 · cook budget 100 µs

Writes measurements as attributes: primitive `area`, `perimeter` and normal `N`, and point `curvature` (mean or Gaussian).

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
| `area` | Area | toggle | `true` |  | Writes the primitive attribute `area` (polygon area; 0 for polylines). |
| `perimeter` | Perimeter | toggle | `false` |  | Writes the primitive attribute `perimeter` (polygon perimeter, or polyline length). |
| `normals` | Normals | toggle | `false` |  | Writes the primitive attribute `N`: each polygon's unit normal. |
| `curvature` | Curvature | menu | `"mean"` | `none`, `mean`, `gaussian` | Writes the point attribute `curvature`. mean: positive where the surface bulges out (1/r on a sphere of radius r), negative in hollows; gaussian: positive on domes, negative on saddles, zero on flat or cylindrical parts (1/r² on a sphere). Points on open borders get 0 (mean) or the border's geodesic turning per area (gaussian). |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `sizes`
