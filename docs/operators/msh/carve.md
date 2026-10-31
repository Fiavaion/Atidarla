# Carve (`msh.carve`)

MSH · version 1 · cook budget 100 µs

Keeps the part of each polyline between two fractions of its length, or the part of each polygon whose `uv` u or v lies in a range. Cuts interpolate every point attribute and stay welded.

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
| `domain` | Domain | menu | `"length"` | `length`, `u`, `v` | length: polylines, by the fraction of each one's length (polygons pass through); u or v: polygons, by that texture coordinate of `uv` (polylines pass through). |
| `start` | Start | float | `0.0` |  | Where the kept part begins (0 is the start of the curve, or that uv value). |
| `end` | End | float | `1.0` |  | Where the kept part ends; an end below the start keeps nothing. |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.atidarla/) — `grow`
