# Subdivide (`msh.subdivide`)

MSH · version 1 · cook budget 100 µs

Smooths polygons by Catmull-Clark subdivision: each level splits every polygon into quads and rounds the shape (open borders stay as curves). Attributes are interpolated.

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
| `levels` | Levels | int | `1` | 0 – 6 | Times each polygon is split (each level turns every polygon into quads, about four times as many). |

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.atidarla/) — `soften`
- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `smooth`
- [Mesh Primitives](../../../examples/mesh-primitives.atidarla/) — `round_box`
