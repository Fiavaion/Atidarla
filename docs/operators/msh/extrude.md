# Extrude (`msh.extrude`)

MSH · version 1 · cook budget 100 µs

Pushes polygons out along their normals and adds side walls, each polygon on its own (with an inset) or as connected regions. The moved polygons are the group `extrude_front`, the walls `extrude_side`.

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
| `group` | Group | string | `""` |  | The primitive group to extrude (empty: every polygon). |
| `mode` | Mode | menu | `"individual"` | `individual`, `connected` | individual: each polygon on its own, with side walls on all its edges; connected: neighbouring polygons move together and only the region's outline gets walls. |
| `distance` | Distance | float | `0.2` |  | How far the polygons move along their normals (negative: inwards). |
| `inset` | Inset | float | `0.0` | 0 – 1 | individual: shrinks each moved polygon towards its centre (0: none, 1: to a point). |

## Platforms

All platforms.

## Examples

- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `tower`
