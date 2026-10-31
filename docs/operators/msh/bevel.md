# Bevel (`msh.bevel`)

MSH · version 1 · cook budget 100 µs

Bevels sharp edges: each becomes a flat chamfer or a rounded strip of segments, with patches where strips meet at a corner. Selected by angle and an optional point group; edges at open borders or non-manifold points stay. The new polygons are the group `bevel`.

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
| `offset` | Offset | float | `0.05` | ≥ 0 | How far each polygon is pulled back from a bevelled edge (limited to just under half the shortest edge at a bevelled corner). |
| `segments` | Segments | int | `1` | 1 – 64 | Polygons across each bevel: 1 is a flat chamfer, more round it. |
| `angle` | Angle | float | `30.0` | 0 – 180 | Only edges where the two polygons' normals differ by more than this many degrees are bevelled. |
| `group` | Group | string | `""` |  | Only edges with both points in this point group are bevelled (empty: all sharp edges). |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `round`
