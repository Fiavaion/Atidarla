# Convex Hull (`msh.convex_hull`)

MSH · version 1 · cook budget 100 µs

The smallest convex mesh around the input's points (quickhull): outward triangles, the points' attributes kept. Flat inputs give one polygon.

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
| `group` | Group | string | `""` |  | Only the points of this point group (empty: every point). |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.atidarla/) — `hull`
