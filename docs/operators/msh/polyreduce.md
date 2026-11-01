# PolyReduce (`msh.polyreduce`)

MSH · version 1 · cook budget 100 µs

Reduces the polygon count while keeping the shape: quadric error edge collapses on triangles, down to a share of the triangles or until the error passes a tolerance. Borders can be kept.

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
| `percent` | Keep (%) | float | `25.0` | 0 – 100 | Share of the triangles to keep. |
| `tolerance` | Tolerance | float | `0.0` | ≥ 0 | Stops earlier once the cheapest collapse would move the surface by more than about this distance (0: no limit). |
| `keep_boundary` | Keep Boundary | toggle | `true` |  | Open edges (holes and borders) do not move. |

## Platforms

All platforms.

## Examples

- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `reduce`
