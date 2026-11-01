# Normals (`msh.normals`)

MSH · version 1 · cook budget 100 µs

Computes normals `N`: smooth point normals with a cusp angle that keeps sharp edges, flat (faceted) normals, or one normal per primitive.

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
| `mode` | Mode | menu | `"smooth"` | `smooth`, `flat`, `prim` | smooth: point normals averaged over the polygons around each point (weighted by area), split where the cusp angle says; flat: every polygon gets its own points with its own normal (faceted look); prim: a normal per polygon as a primitive attribute, points untouched. |
| `cusp` | Cusp Angle | float | `180.0` | 0 – 180 | smooth: edges where neighbouring polygons differ by more than this many degrees stay sharp (their points are split). 180 never splits. |
| `reverse` | Reverse | toggle | `false` |  | Points the normals the other way (polygons are not changed). |

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.lasair/) — `edges`
- [Mesh Shaping](../../../examples/mesh-shaping.lasair/) — `crease`
- [VAT Playback](../../../examples/mesh-vat.lasair/) — `flat`
