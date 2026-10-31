# Iso Surface (`msh.iso_surface`)

MSH · version 1 · cook budget 100 µs

Builds a closed mesh where a field crosses zero (marching tetrahedra): blended spheres around the input points (radius × `pscale`), or an analytic sphere, box, torus or gyroid. Writes `N` from the field.

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
| `source` | Source | menu | `"points"` | `points`, `sphere`, `box`, `torus`, `gyroid` | points: a sphere around every input point, blended together; the others: an analytic shape (no input needed). |
| `radius` | Radius | float | `0.1` | ≥ 0 | points: each sphere's radius (times the point's `pscale`); sphere and torus: the radius. Points mode refuses a cook whose spheres together visit over 200 million grid nodes (lower Radius, Resolution or the point count). |
| `blend` | Blend | float | `0.05` | ≥ 0 | points: how far neighbouring spheres melt into each other (a smooth union). |
| `thickness` | Thickness | float | `0.15` | ≥ 0 | torus: tube radius; gyroid: wall thickness. |
| `size` | Size | vec3 | `[2.0,2.0,2.0]` |  | box: its extent; gyroid: the box it fills. |
| `frequency` | Frequency | float | `6.0` | ≥ 0 | gyroid: cells per unit (times 2π). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of an analytic shape. |
| `resolution` | Resolution | int | `64` | 4 – 256 | Cells along the longest side of the bounds (detail and cost grow with its cube). |
| `offset` | Offset | float | `0.0` |  | Grows (positive) or shrinks (negative) the surface by this distance. |

## Platforms

All platforms.

## Examples

- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.atidarla/) — `blob`
