# Chromatic Aberration

Splits the colour channels apart: red moves one way, blue the other, green stays. Edges get
coloured fringes, like a cheap lens, a misaligned projector or an old VHS tape.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image to split. |
| `out` | IMG | The image with its channels apart. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Amount | 0.008 | Distance red and blue move, as a fraction of the image. 0.003 is subtle, 0.03 is broken. Negative swaps the sides. |
| Direction | 1.0, 0.5, 0, 1 | The split direction as a colour: red above 0.5 moves sideways, green above or below 0.5 moves up or down (0.5 is no movement). `1, 1` splits diagonally. |

## Tips

- Bind Amount to an LFO or an audio envelope for a glitch that pulses with the music
  (`docs/expressions.md` for expressions, the binding menu for channels).
- The split is uniform across the image; for a lens-like split that grows towards the edges,
  feed a radial Ramp into a copy of the Pack (unlock it first).

## Inside

Three Channel Mix nodes isolate red, green and blue; two Displace nodes shift red and blue by the
Direction colour (blue by minus red's amount, through an expression); two Composites add the
channels back together.

## Versions

- 1.0.0 (2026-10-18): first version.