# Vignette

Darkens the edges of an image, or tints them towards a colour, so the eye goes to the centre.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image. |
| `out` | IMG | The image with its edges darkened. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Amount | 0.8 | Strength; 0 leaves the image alone. |
| Softness | 0.6 | Falloff curve: low keeps a clear centre, high darkens from the centre out. |
| Size | 0 | Moves the falloff in (negative) or out (positive). |
| Edge Colour | black | The colour the edges fade to. A deep blue or red tints instead of darkening. |

## Inside

Ramp (radial, white to Edge Colour) → Level (gamma) → Composite (multiply) with the input.

## Versions

- 1.0.0 (2026-10-18): first version.