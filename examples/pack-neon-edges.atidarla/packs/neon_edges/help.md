# Neon Edges

Traces the outlines of an image, widens and brightens them, saturates their colour and screens
them back over the image: text, shapes and footage get a neon-sign look.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image to outline. |
| `out` | IMG | The image with glowing outlines. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Strength | 3 | Edge detection gain. |
| Width | 5 px | Blur of the outlines: wider and softer. |
| Glow | 5 | Brightness of the outlines. |
| Hue | 0° | Rotates the outline colours. |
| Saturation | 1.6 | Colour intensity of the outlines. |
| Mix | 1 | How much of the outlines is screened over the image; 0 is the image alone. |

## Tips

- On a black background with Mix 1 you get the outlines alone.
- Bind Hue to an LFO for colour-cycling signs; bind Glow to an Audio Pulse's `level` to flash
  on the beat.

## Inside

Edge (colour mode) → Blur → Level → HSV Adjust → Composite (screen) over the input.

## Versions

- 1.0.0 (2026-10-18): first version.