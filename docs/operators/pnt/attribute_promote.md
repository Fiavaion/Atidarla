# Attribute Promote (`pnt.attribute_promote`)

PNT · version 1 · cook budget 300 µs

Reduces a point attribute over all the points (min, max, sum or average, component by component) and writes the result to every point as Result, with the source's type (average and sum of integers are rounded). Use it to normalise by the largest value, centre on the average position, or scale by a total. Works on GPU-counted geometry (only live points count).

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
| `name` | Name | string | `"P"` |  | The point attribute reduced. |
| `operation` | Operation | menu | `"average"` | `min`, `max`, `sum`, `average` | min, max: the smallest, largest value of each component; sum: their total; average: the total over the live point count. |
| `result` | Result | string | `"promoted"` |  | The attribute written on every point (not Name). |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.lasair/) — `flock_centre`
