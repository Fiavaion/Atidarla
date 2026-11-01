# Merge (`msh.merge`)

MSH · version 1 · cook budget 100 µs

Combines up to four geometries into one. Attributes and groups are joined by name; missing values are zero (one for `Cd` and `pscale`).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in0` | Geo | optional |
| `in1` | Geo | optional |
| `in2` | Geo | optional |
| `in3` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

None.

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.lasair/) — `rings`, `both`
- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `all`
- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `shapes`, `scene`
- [Mesh Shaping](../../../examples/mesh-shaping.lasair/) — `all`
