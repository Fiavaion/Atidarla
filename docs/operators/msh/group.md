# Group (`msh.group`)

MSH · version 1 · cook budget 100 µs

Makes a point or primitive group: elements inside a box, facing a direction, every n-th in an index range, or all; combined with an existing group by union, intersection or subtraction.

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
| `name` | Name | string | `"group1"` |  | The group's name. |
| `class` | Class | menu | `"prim"` | `prim`, `point` | Whether the group holds primitives or points. |
| `method` | Method | menu | `"box"` | `box`, `normal`, `range`, `all` | box: elements (points, or primitive centres) inside a box; normal: elements facing a direction; range: every n-th element of an index range; all: everything. |
| `box_centre` | Box Centre | vec3 | `[0.0,0.0,0.0]` |  | box: centre. |
| `box_size` | Box Size | vec3 | `[1.0,1.0,1.0]` |  | box: extent. |
| `direction` | Direction | vec3 | `[0.0,1.0,0.0]` |  | normal: direction. |
| `angle` | Spread Angle | float | `45.0` | 0 – 180 | normal: most degrees between an element's normal and the direction. |
| `range_start` | Range Start | int | `0` | ≥ 0 | range: first index. |
| `range_end` | Range End | int | `-1` |  | range: last index (negative counts from the end). |
| `range_step` | Range Step | int | `2` | ≥ 1 | range: every n-th element. |
| `combine` | Combine | menu | `"replace"` | `replace`, `union`, `intersect`, `subtract` | How the new selection combines with an existing group of that name. |

## Platforms

All platforms.

## Examples

- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `top`, `upper`
