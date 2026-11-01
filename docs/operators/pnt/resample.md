# Resample (`pnt.resample`)

PNT · version 1 · cook budget 100 µs

Rebuilds every polyline with evenly spaced points, about a spacing apart or a fixed count per curve (closed primitives too, as outlines, when asked). Point attributes are interpolated; curve ends stay where they were.

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
| `mode` | Mode | menu | `"spacing"` | `spacing`, `count` | spacing: points about `spacing` apart along each curve (adjusted so they divide it evenly); count: exactly `count` points per curve. |
| `spacing` | Spacing | float | `0.1` | ≥ 0.0001 | spacing: the distance between neighbouring points along the curve. |
| `count` | Count | int | `10` | 2 – 100000 | count: points per curve, both ends included (closed curves: at least 3). |
| `polygons` | Polygons Too | toggle | `false` |  | Resamples closed primitives as closed outlines as well; off, they pass through unchanged. |

## Platforms

All platforms.

## Examples

- [PNT Outputs](../../../examples/pnt-outputs.lasair/) — `even`
