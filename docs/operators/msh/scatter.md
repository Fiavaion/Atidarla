# Scatter (`msh.scatter`)

MSH · version 1 · cook budget 100 µs

Seeded random points on the surface, evenly by area, with the surface normal `N`, an `id` and the surface's other attributes interpolated: points for PNT operators or Iso Surface.

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
| `count` | Count | int | `1000` | 0 – 8000000 | Number of points. |
| `seed` | Seed | int | `1` | ≥ 0 | Random seed: the same seed gives the same points. |
| `group` | Group | string | `""` |  | Scatter only on this primitive group (empty: every polygon). |
| `pscale` | Point Scale | float | `0.0` | ≥ 0 | Writes `pscale` with this value when above 0 (Iso Surface multiplies its radius by it). |

## Platforms

All platforms.

## Examples

- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.atidarla/) — `seeds`
