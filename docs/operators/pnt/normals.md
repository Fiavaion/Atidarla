# Normals (`pnt.normals`)

PNT · version 1 · cook budget 800 µs

Estimates surface normals `N` for a point cloud: the direction in which each point's neighbours (within Radius) spread least, from their covariance, turned outward from a centre or toward a direction. Neighbours come from a GPU grid hash.

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
| `radius` | Radius | float | `0.2` | ≥ 0.000001 | Points within this distance are neighbours (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `128` | 1 – 256 | A bound on the work per point: at most this many neighbours are used. Where more are within Radius the first found (in a fixed order, cell by cell) are used, which biases the result: raise it or lower Radius. |
| `orient` | Orient | menu | `"outward"` | `outward`, `toward`, `none` | outward: normals point away from Centre (closed shapes around it); toward: they point along Direction (open sheets); none: as computed (either side). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | outward: the point inside the shape. |
| `direction` | Direction | vec3 | `[0.0,0.0,1.0]` |  | toward: the side normals face; also the normal of a point with fewer than three neighbours. |

## Platforms

All platforms.

## Examples

- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `normals`
