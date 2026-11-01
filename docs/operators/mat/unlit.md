# Unlit (`mat.unlit`)

MAT · version 1 · cook budget 20 µs

A flat colour with no lighting: Colour, times `Cd` and times the `map` image (sampled at the `uv` attribute) when they are there. Use it for constant colours, vertex colours and textures shown as they are.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `map` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Surface colour; alpha below 1 is screen-door transparency when the renderer antialiases. |
| `use_cd` | Use Cd | toggle | `true` |  | Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one. |

## Platforms

All platforms.

## Examples

- [Render Textures](../../../examples/render-textures.lasair/) — `screen`
