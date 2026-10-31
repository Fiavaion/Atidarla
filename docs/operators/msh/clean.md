# Clean (`msh.clean`)

MSH · version 1 · cook budget 100 µs

Repairs geometry: welds duplicate points, removes degenerate and duplicate primitives and unused points, and makes polygon winding consistent (closed parts facing outward).

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
| `fuse` | Fuse Points | toggle | `true` |  | Welds points closer than Distance into one (the lowest-numbered keeps its attributes). |
| `distance` | Distance | float | `0.00001` | ≥ 0 | Fuse Points: points closer than this are welded (0: only exact duplicates). |
| `degenerate` | Remove Degenerate | toggle | `true` |  | Removes repeated corners, polygons with fewer than three distinct points or no area (see Min Area), and polylines with fewer than two points. |
| `min_area` | Min Area | float | `0.0` | ≥ 0 | Remove Degenerate: polygons with at most this area count as degenerate. |
| `duplicates` | Remove Duplicates | toggle | `true` |  | Removes primitives using the same points as an earlier one (in either direction); the first is kept. |
| `winding` | Fix Winding | toggle | `true` |  | Flips polygons so neighbours agree on their direction; closed parts are turned to face outward, open parts follow the direction most of their polygons had. |
| `unused` | Remove Unused Points | toggle | `true` |  | Removes points no primitive uses (when there are primitives). |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `tidy`
