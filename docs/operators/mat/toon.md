# Toon (`mat.toon`)

MAT · version 1 · cook budget 20 µs

Cel shading: the light's diffuse term in a few hard bands between Shadow and Colour, a hard highlight and a rim light along silhouettes.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `colour` | Colour | colour | `[0.95,0.55,0.3,1.0]` |  | Colour where the light falls fully. |
| `shadow` | Shadow | colour | `[0.25,0.1,0.2,1.0]` |  | Colour where the light does not fall. |
| `bands` | Bands | int | `3` | 1 – 16 | Number of steps from shadow to light. |
| `highlight` | Highlight | float | `0.3` | 0 – 1 | Size of the hard specular spot; 0 turns it off. |
| `rim` | Rim | float | `0.5` | 0 – 2 | Strength of the rim light along silhouettes. |
| `rim_width` | Rim Width | float | `0.3` | 0 – 1 | How far the rim reaches in from the silhouette. |
| `use_cd` | Use Cd | toggle | `false` |  | Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one. |

## Platforms

All platforms.

## Examples

- [Render Wire and Toon](../../../examples/render-wire-toon.atidarla/) — `cel`
