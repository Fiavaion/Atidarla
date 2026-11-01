# Tube (`msh.tube`)

MSH · version 1 · cook budget 100 µs

A cylinder or cone along Y, optionally capped (closed), with smooth normals `N`. A radius of 0 makes a cone point.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `radius_bottom` | Bottom Radius | float | `0.5` | ≥ 0 | Radius at the bottom; 0 makes a cone point. |
| `radius_top` | Top Radius | float | `0.5` | ≥ 0 | Radius at the top; 0 makes a cone point. |
| `height` | Height | float | `1.0` |  | Height along Y. |
| `rows` | Rows | int | `2` | 2 – 4096 | Rings from bottom to top (2: one band). |
| `columns` | Columns | int | `24` | 3 – 4096 | Segments around. |
| `caps` | Caps | toggle | `true` |  | Closes each end with one polygon (an end of radius 0 needs none). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |

## Platforms

All platforms.

## Examples

- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `cone`
