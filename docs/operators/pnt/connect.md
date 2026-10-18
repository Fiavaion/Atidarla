# Connect (`pnt.connect`)

PNT · version 1 · cook budget 800 µs

Links each point to its nearest neighbours within Radius with lines (polyline primitives of two points; Render Points draws them with Draw set to lines), and writes `near` (the nearest neighbour's number, or the point's own) and `neardist` (its distance, or Radius when there is none). Neighbours come from a GPU grid hash.

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
| `radius` | Radius | float | `0.15` | ≥ 0.000001 | Points within this distance are neighbours (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `64` | 1 – 256 | A bound on the work per point: at most this many neighbours are used. Where more are within Radius the first found (in a fixed order, cell by cell) are used, which biases the result: raise it or lower Radius. |
| `links` | Links | int | `2` | 1 – 8 | Lines from each point to its nearest neighbours. |
| `unique` | Unique | toggle | `true` |  | Links a pair once: each point links only to neighbours with a higher point number. |

## Platforms

All platforms.

## Examples

- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `links`
