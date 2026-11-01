# Bevel (`msh.bevel`)

MSH · version 1 · cook budget 100 µs

Bevels sharp edges: each becomes a flat chamfer or a rounded strip of segments, with patches where strips meet at a corner. A strip that ends inside the mesh cuts across the third polygon there, or, where four or more polygons meet, closes its end with triangles to the point. Uneven corner patches and strips twisted by more than 0.2% of their size become triangles. Selected by angle and an optional point group. A strip that reaches an open border runs to it: the border stays open and gains points where the strip meets it. Border edges themselves (one polygon, so no angle to round) are never bevelled, and edges at non-manifold points stay. The new polygons are the group `bevel`.

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

- [Mesh Shaping](../../../examples/mesh-shaping.lasair/) — `round`
