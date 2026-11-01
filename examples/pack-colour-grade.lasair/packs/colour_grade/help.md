# Colour Grade

The everyday colour controls in one node, to put at the end of a chain: exposure, contrast,
gamma, saturation, hue and a colour tint. Every control starts neutral, so dropping it in changes
nothing until you move one.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image. |
| `out` | IMG | The graded image. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Exposure | 1 | Multiplies the brightness: 2 is twice as bright. |
| Contrast | 1 | Above 1 spreads the tones apart from mid-grey, below 1 flattens them. |
| Gamma | 1 | Above 1 lifts the mid-tones, below 1 darkens them; black and white stay. |
| Saturation | 1 | Colour intensity: 0 is black and white. |
| Hue | 0° | Rotates every colour round the colour wheel. |
| Tint | warm orange | A colour filter, like a gel on a light. |
| Tint Amount | 0 | How much of the Tint is applied. |

## Tips

- A warm Tint at 0.3 with Saturation 1.2 and Contrast 1.1 is a quick sunny look; a pale blue
  Tint with Saturation 0.7 a cold one.
- Bind Hue to an LFO for slowly cycling colours, or Exposure to a Beat Flash's `trigger`.
- Order matters: exposure and contrast first, then saturation and hue, then the tint.

## Inside

Level (Exposure, Contrast, Gamma) → HSV Adjust (Saturation, Hue) → Composite (multiply) with a
Constant in the Tint colour, at Tint Amount.

## Versions

- 1.0.0 (2026-10-22): first version.
