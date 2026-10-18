# Sort (`pnt.sort`)

PNT · version 1 · cook budget 2000 µs

Reorders points: along an axis, by distance from a point (far first suits Render Points' `over` blending), by an attribute, randomly (seeded) or reversed. Equal keys keep their order, so the result is the same on every run. Every attribute follows; a few milliseconds for millions of points (not a per-frame step at that size).

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
| `key` | Key | menu | `"axis"` | `axis`, `distance`, `attribute`, `random`, `reverse` | axis: position along Axis; distance: from Point; attribute: a component of Attribute; random: a seeded shuffle (by `id` when the points have one); reverse: the order backwards. |
| `axis` | Axis | vec3 | `[0.0,0.0,1.0]` |  | axis: the direction sorted along. |
| `point` | Point | vec3 | `[0.0,0.0,5.0]` |  | distance: the point distances are measured from. |
| `attribute` | Attribute | string | `"age"` |  | attribute: the point attribute. |
| `component` | Component | int | `0` | 0 – 3 | attribute: which component. |
| `descending` | Descending | toggle | `false` |  | Largest first (for distance: farthest first). |
| `seed` | Seed | int | `0` | ≥ 0 | random: selects a different order. |

## Platforms

All platforms.

## Examples

- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `back_to_front`
