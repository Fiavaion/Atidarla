# Points (`pnt.points`)

PNT · version 1 · cook budget 150 µs

A point source: a grid, random points in a box, a line or a circle, made on the GPU. Writes `P`, `Cd` (colour) and `id` (0, 1, 2, …).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `shape` | Shape | menu | `"grid"` | `grid`, `random`, `line`, `circle` | grid: rows and columns in the XY plane; random: uniform in a box; line: along X; circle: in the XY plane. |
| `count` | Count | int | `10000` | 0 – 16000000 | Number of points (a grid uses the nearest rows × columns at or above it, trimmed to the count). |
| `size` | Size | vec3 | `[2.0,2.0,2.0]` |  | Extent along X, Y and Z (a circle's diameters along X and Y). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |
| `seed` | Seed | int | `1` | ≥ 0 | Random seed (random shape and random colours); the same seed gives the same points. |
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Colour written to `Cd`. |
| `random_colour` | Random Colour | toggle | `false` |  | Multiplies the colour by a random hue per point. |

## Platforms

All platforms.

## Examples

- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `grid`
- [Points](../../../examples/points.atidarla/) — `cloud`, `grid`
- [Render Wire and Toon](../../../examples/render-wire-toon.atidarla/) — `cloud`
