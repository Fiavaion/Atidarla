# Delete (`pnt.delete`)

PNT · version 1 · cook budget 200 µs

Removes the points a rule selects: members of a group, points inside a box or sphere, or points whose attribute passes a comparison. Invert keeps them instead, so two Deletes split a geometry. Every attribute follows; the count stays on the GPU.

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
| `rule` | Rule | menu | `"box"` | `group`, `box`, `sphere`, `attribute` | group: members of a point group; box, sphere: points inside the region; attribute: points where the comparison holds. |
| `group` | Group | string | `""` |  | group: the point group's name. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | box, sphere: the centre. |
| `size` | Size | vec3 | `[1.0,1.0,1.0]` |  | box: the extent along X, Y and Z. |
| `radius` | Radius | float | `0.5` | ≥ 0 | sphere: the radius. |
| `attribute` | Attribute | string | `"P"` |  | attribute: the point attribute compared. |
| `component` | Component | int | `0` | 0 – 3 | attribute: which component (0 x, 1 y, 2 z, 3 w). |
| `compare` | Compare | menu | `">"` | `<`, `<=`, `>`, `>=`, `==`, `!=` | attribute: the comparison, attribute on the left. |
| `value` | Value | float | `0.0` |  | attribute: the value compared with. |
| `fraction` | Fraction | float | `0.5` | 0 – 1 | random: the fraction of points selected. |
| `nth` | Nth | int | `2` | ≥ 1 | every_nth: one point in this many. |
| `offset` | Offset | int | `0` | ≥ 0 | every_nth: shifts which points are picked. |
| `first` | First | int | `0` | ≥ 0 | range: the first point number. |
| `count` | Count | int | `100` | ≥ 0 | range: how many points from the first. |
| `by_id` | By ID | toggle | `true` |  | random, every_nth, range: number points by their `id` attribute when they have one (a particle keeps its fate from frame to frame), else by position in the list. |
| `seed` | Seed | int | `0` | ≥ 0 | random: selects a different set. |
| `invert` | Invert | toggle | `false` |  | Deletes the points the rule does not select instead (keeps the selection: a split). |

## Platforms

All platforms.

## Examples

- [PNT Capture](../../../examples/pnt-capture.atidarla/) — `bright_only`
- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `hole`, `pick`
- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `holes`
