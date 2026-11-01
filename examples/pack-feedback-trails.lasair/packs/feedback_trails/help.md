# Feedback Trails

Leaves trails behind whatever moves in the input. Each frame the previous result is zoomed,
turned, faded and hue-shifted, then combined with the new input, so moving shapes smear into
spirals and tunnels of colour.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image that leaves trails. Works best with something moving on black. |
| `out` | IMG | The input with its trails. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Decay | 0.95 | Share of the trail kept each frame. 0.8 gives short trails, 0.98 long ones. |
| Zoom | 1.02, 1.02 | Scale per frame. Above 1 flies outwards, below 1 falls inwards; unequal values stretch. |
| Spin | 0.5° | Turn per frame. |
| Hue Shift | 3° | Hue rotation per frame: the trail cycles through colours as it ages. |
| Blend | maximum | `maximum` keeps the brightest pixel, `add` builds up light (raise Decay carefully), `over` draws the input on top. |
| Reset | off | Clears the trail while on. |

## Tips

- Bind Spin or Zoom to an LFO for breathing tunnels, or to an audio level so the trail
  jumps with the music.
- Decay close to 1 with Blend `add` saturates to white quickly; lower Decay or the input's
  brightness.
- The trail keeps state from frame to frame, so an offline render started part-way differs from
  the same frames of a longer render (see `docs/render.md`).

## Inside

Feedback → Transform (zoom, spin) → Level (decay) → HSV Adjust (hue shift) → Composite with the
input, fed back into Feedback's loop.

## Versions

- 1.0.0 (2026-10-18): first version.