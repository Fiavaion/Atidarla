# Matcap (`mat.matcap`)

MAT · version 1 · cook budget 20 µs

A lit-sphere image (`matcap` input) looked up by the surface normal as the camera sees it, so a painted or photographed sphere gives the whole shape its look with no lights. Without an image it shows a soft top light.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `matcap` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `tint` | Tint | colour | `[1.0,1.0,1.0,1.0]` |  | Multiplies the matcap colour. |
| `use_cd` | Use Cd | toggle | `false` |  | Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one. |

## Platforms

All platforms.

## Examples

- [Render Textures](../../../examples/render-textures.atidarla/) — `clay`
