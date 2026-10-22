# Duotone

Recolours an image in two colours, the poster and record-sleeve look: everything dark becomes the
Shadows colour, everything bright the Highlights colour, and the tones in between a gradient from
one to the other. Footage, noise and text all take it well.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image. |
| `out` | IMG | The image in two colours. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Shadows | deep violet | The colour dark areas become. |
| Highlights | warm cream | The colour bright areas become. |
| Contrast | 1.2 | Contrast before recolouring: higher pushes more of the image to the two pure colours, lower keeps more of the gradient. |
| Balance | 0 | Moves the point where one colour turns into the other, so more of the image takes one colour. |
| Mix | 1 | 0 shows the original, 1 the duotone; in between tints the original. |

## Tips

- Swap the two colours for a negative-like look.
- Bind Balance to an LFO or an audio level for colour that pulses with the music.
- Contrast 3 or more gives a near-posterised, two-colour print.

## Inside

Level (contrast) → Lookup through a horizontal Ramp from Shadows to Highlights → Composite over
the original (Mix).

## Versions

- 1.0.0 (2026-10-22): first version.
