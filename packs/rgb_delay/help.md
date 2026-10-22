# RGB Delay

Splits an image into red, green and blue and delays green and blue behind red. Still parts look
normal; anything that moves leaves a trail of red, green and blue copies of itself, like a broken
video signal or a three-colour print out of register.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image. Works best with moving shapes or footage. |
| `out` | IMG | The image with its colours delayed. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Delay | 4 frames | How many frames green lags behind red; blue lags twice as far. 0 is no effect, 30 splits a movement into three separate copies. |
| Mix | 1 | 0 shows the original image, 1 the delayed channels. |

## Tips

- Bind Mix to a Beat Flash's `trigger` (through an expression that maps it to 0–1) so the split
  only appears on the beat.
- Pure white moving shapes on black show the effect most clearly; slow movements need a longer
  Delay.
- The delay keeps frames from earlier in the render, so an offline render started part-way
  differs from the same frames of a longer render (see `docs/render.md`).

## Inside

Three Channel Mix nodes isolate red, green and blue; two Caches delay green by Delay and blue by
twice Delay (an expression); two Composites add the channels back; a third mixes the result over
the original.

## Versions

- 1.0.0 (2026-10-22): first version.
