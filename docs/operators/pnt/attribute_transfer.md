# Attribute Transfer (`pnt.attribute_transfer`)

PNT · version 1 · cook budget 900 µs

Carries an attribute from the points of `from` to the nearby points of `in` (colours painted by moving brushes, masks from another cloud): the nearest source within Radius, or a distance-weighted average of every source within it, blended by distance and Amount. Points with no source within Radius keep their value. Searches a GPU grid hash.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `from` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name` | Name | string | `"Cd"` |  | The attribute carried from `from` (created on `in` with its type when missing). |
| `method` | Method | menu | `"nearest"` | `nearest`, `average` | nearest: the value of the nearest point within Radius; average: the values of every point found within Radius, weighted by a smooth falloff of distance. |
| `radius` | Radius | float | `0.25` | ≥ 0.000001 | Points farther than this are not found (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `32` | 1 – 256 | A bound on the work per point: at most this many points within Radius are visited, cell by cell in a fixed order; where more are within Radius the nearest may be missed: raise it or lower Radius. |
| `fade` | Fade | toggle | `true` |  | Blends by distance: full at the source point, nothing at Radius (off: full wherever a source is found). |
| `amount` | Amount | float | `1.0` |  | Blends from the point's own value (0) to the transferred one (1). |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `paint`
