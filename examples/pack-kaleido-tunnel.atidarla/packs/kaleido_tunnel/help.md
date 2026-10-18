# Kaleidoscope Tunnel

A generator that runs by itself: coloured noise drifts through a slowly turning kaleidoscope, is
mirrored, tiled, hue-cycled and glows. Drop it in as a background, or blend two with different
settings.

## Outputs

| Socket | Type | What |
|---|---|---|
| `out` | IMG | The tunnel, at the project resolution. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Speed | 0.3 | Drift through the noise per second (negative goes backwards). |
| Segments | 8 | Mirrored segments of the kaleidoscope (1–32). |
| Scale | 2.5 | Noise feature size: higher is finer. |
| Repeat | 2, 2 | Tiles across and down. |
| Hue Speed | 0.05 | Colour swing, in cycles per second. |
| Glow | 0.8 | Bloom intensity. |

## Tips

- Segments is an integer parameter: drive it with an expression such as
  `6 + 2 * floor(time / 4 % 3)` for a pattern that changes every four seconds.
- Bind Speed to an audio level to rush through the tunnel on loud passages.

## Inside

Constant → Speed (drift) → Noise z; Timer → Kaleidoscope rotation; LFO → HSV hue; Noise →
Kaleidoscope → Mirror → Tile → HSV Adjust → Bloom. The same network as the
`kaleidoscope-tunnel` example, packed.

## Versions

- 1.0.0 (2026-10-18): first version.