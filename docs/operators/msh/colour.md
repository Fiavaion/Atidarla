# Colour (`msh.colour`)

MSH · version 1 · cook budget 100 µs

Writes the point colour `Cd`: a constant, or from the normals, texture coordinates, position in the bounds, or a seeded random colour per point or per primitive, times a tint.

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
| `mode` | Mode | menu | `"constant"` | `constant`, `normal`, `uv`, `position`, `random`, `random_prim` | constant: the tint; normal: N mapped to 0-1 (computed when missing); uv: (u, v, 0); position: where each point sits in the bounds; random: a seeded colour per point; random_prim: per primitive (its points are split so each primitive is one colour). |
| `colour` | Tint | colour | `[1.0,1.0,1.0,1.0]` |  | The colour, or the multiplier of the others. |
| `seed` | Seed | int | `1` | ≥ 0 | random: the seed. |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.lasair/) — `uv_colour`, `piece_colour`
- [Mesh Files](../../../examples/mesh-files.lasair/) — `vase_colour`, `loft_colour`
- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `smooth_colour`, `cut_colour`, `reduce_colour`
- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `tint`
- [VAT Playback](../../../examples/mesh-vat.lasair/) — `ripple_colour`
