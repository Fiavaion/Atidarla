# Copy to Points (`pnt.copy`)

PNT · version 1 · cook budget 200 µs

Copies the `source` geometry onto every point of `points`: copy t of the source is scaled by Scale and the point's `pscale`, turned by its `orient` (a quaternion) or so the source's +Z follows its `N`, and moved to its `P`. Every source attribute is copied; `copynum` gives the copy's number, and `Cd` can take the point's colour.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `source` | Geo | optional |
| `points` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `scale` | Scale | float | `1.0` |  | Scales every copy. |
| `use_pscale` | Use pscale | toggle | `true` |  | Scales each copy by the point's `pscale` when it has one. |
| `orient` | Orient | menu | `"orient"` | `none`, `orient`, `normal` | none: copies keep the source's orientation; orient: by the point's `orient` quaternion; normal: the source's +Z turns to the point's `N` (when the attribute exists). |
| `point_colour` | Point Colour | toggle | `true` |  | Multiplies each copy's `Cd` by the point's `Cd` (when the points have one). |

## Platforms

All platforms.

## Examples

- [PNT Deformers](../../../examples/pnt-deformers.lasair/) — `rings`
