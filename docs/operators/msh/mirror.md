# Mirror (`msh.mirror`)

MSH · version 1 · cook budget 100 µs

Reflects the geometry across a plane, with or without the original; the seam can be welded so a half model becomes one closed piece. The reflected primitives are the group `mirrored`.

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
| `origin` | Origin | vec3 | `[0.0,0.0,0.0]` |  | A point on the mirror plane. |
| `normal` | Normal | vec3 | `[1.0,0.0,0.0]` |  | The mirror plane's normal (the default mirrors X to -X). |
| `keep_original` | Keep Original | toggle | `true` |  | Outputs the original and its mirror image; off, only the mirror image. |
| `merge_seam` | Merge Seam | toggle | `true` |  | Keep Original: points within Seam Distance of the plane are moved onto it and shared by both halves, and polygons lying in the plane (which would be inside) are removed. |
| `distance` | Seam Distance | float | `0.0001` | ≥ 0 | Merge Seam: how close to the plane a point must be to join the seam. |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.lasair/) — `whole`
