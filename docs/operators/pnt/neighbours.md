# Neighbours (`pnt.neighbours`)

PNT · version 1 · cook budget 800 µs

The k nearest neighbours of every point within Radius (k-NN, at most 4): writes `nbr` (four integers: their point numbers, nearest first, −1 where there are fewer), `nbrdist` (their distances, Radius for missing ones) and `nbrcount` (how many points are within Radius, up to Max Neighbours). Kernels and Attribute Math read them; Connect draws lines to them. Searches a GPU grid hash.

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
| `k` | K | int | `4` | 1 – 4 | How many nearest neighbours to keep (at most 4). |
| `radius` | Radius | float | `0.2` | ≥ 0.000001 | Points farther than this are not found (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `64` | 1 – 256 | A bound on the work per point: at most this many points within Radius are visited, cell by cell in a fixed order; where more are within Radius the nearest may be missed: raise it or lower Radius. |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `crowding`
