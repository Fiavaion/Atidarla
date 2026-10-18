# Boolean (`msh.boolean`)

MSH · version 1 · cook budget 100 µs

Union, intersection or difference of two closed meshes (BSP trees). The result is welded; pieces from A and B are the groups `from_a` and `from_b`. Up to 200,000 triangles per input; seams may keep T-junctions; normals are not written (add Normals).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `a` | Geo | optional |
| `b` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `operation` | Operation | menu | `"union"` | `union`, `intersect`, `subtract` | union: the space inside A or B; intersect: inside both; subtract: inside A but not B. |

## Platforms

All platforms.

## Examples

- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `cut`
