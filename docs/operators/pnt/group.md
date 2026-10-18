# Group (`pnt.group`)

PNT · version 1 · cook budget 80 µs

Puts points into a named group by a rule (inside a box or sphere, an attribute comparison, a random fraction, every Nth, a range, or another group), as a bit of the `group` attribute. Delete, and other operators' group options, read it.

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
| `name` | Name | string | `"group1"` |  | Name of the point group written (bits in the `group` attribute; at most 32 groups). |
| `rule` | Rule | menu | `"box"` | `group`, `box`, `sphere`, `attribute`, `random`, `every_nth`, `range` | group: members of another group; box, sphere: inside the region; attribute: where the comparison holds; random: a seeded fraction; every_nth: one in Nth; range: Count from First. |
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
| `operation` | Operation | menu | `"replace"` | `replace`, `union`, `intersect`, `subtract` | replace: the group becomes the selection; union: adds it; intersect: keeps only members also selected; subtract: removes the selection. |
| `invert` | Invert | toggle | `false` |  | Uses the points the rule does not select. |

## Platforms

All platforms.

## Examples

No example project uses it yet.
