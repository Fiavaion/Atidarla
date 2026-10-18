# Smooth (`pnt.smooth`)

PNT · version 1 · cook budget 800 µs

Averages a point attribute with its neighbours within Radius, weighted by distance (closer counts more): relaxes positions, blurs colours or any other attribute over a point cloud. Neighbours come from a GPU grid hash of the input positions.

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
| `attribute` | Attribute | string | `"P"` |  | The point attribute smoothed (any type). |
| `radius` | Radius | float | `0.2` | ≥ 0.000001 | Points within this distance are neighbours (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `128` | 1 – 256 | A bound on the work per point: at most this many neighbours are used. Where more are within Radius the first found (in a fixed order, cell by cell) are used, which biases the result: raise it or lower Radius. |
| `strength` | Strength | float | `1.0` | 0 – 1 | How far each value moves toward its neighbourhood's average per iteration. |
| `iterations` | Iterations | int | `1` | 1 – 16 | Repeats the averaging (the neighbourhoods stay those of the input positions). |

## Platforms

All platforms.

## Examples

- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `relax`
