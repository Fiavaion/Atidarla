# Remesh (`msh.remesh`)

MSH · version 1 · cook budget 100 µs

Rebuilds the surface as even triangles of about one edge length (isotropic remeshing: split, collapse, flip, relax, project back), keeping open borders and sharp edges. Attributes are interpolated from the nearest point of the input surface; polylines are dropped.

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
| `edge_length` | Edge Length | float | `0.1` | ≥ 0.000001 | The edge length the triangles aim for. |
| `iterations` | Iterations | int | `5` | 1 – 50 | Rounds of splitting, collapsing, flipping and relaxing. |
| `feature_angle` | Feature Angle | float | `45.0` | 0 – 180 | Edges where the surface bends by more than this many degrees are kept as creases (180: none); open borders are always kept. |
| `project` | Project | toggle | `true` |  | Moves the points back onto the input surface after each relaxation. |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `even`
