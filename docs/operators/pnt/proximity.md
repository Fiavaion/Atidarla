# Proximity (`pnt.proximity`)

PNT · version 1 · cook budget 800 µs

For every point of `in`, the nearest point of `to` within Radius (of `in` itself, other than the point, when `to` is not connected): writes `dist` (its distance, or Radius when there is none) and `nearvec` (the vector to it, or zero). Drive colour, size or a force by how close things are. Searches a GPU grid hash.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `to` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `radius` | Radius | float | `0.5` | ≥ 0.000001 | Points farther than this are not found (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `64` | 1 – 256 | A bound on the work per point: at most this many points within Radius are visited, cell by cell in a fixed order; where more are within Radius the nearest may be missed: raise it or lower Radius. |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `near_brush`
