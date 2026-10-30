# Noise (`img.noise`)

IMG · version 1 · cook budget 400 µs

Fractal gradient noise. Animate `z` to move through it over time.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `scale` | Scale | float | `4.0` | ≥ 0 | Noise features across the image width. |
| `octaves` | Octaves | int | `3` | 1 – 8 | Layers of detail, each twice the frequency of the last. |
| `roughness` | Roughness | float | `0.5` | 0 – 1 | Amplitude ratio between successive octaves. |
| `seed` | Seed | int | `0` |  | Selects a different noise pattern. |
| `z` | Z | float | `0.0` |  | Position along the third axis; drive it with time to animate. |
| `amplitude` | Amplitude | float | `0.5` |  | Output = noise × amplitude + offset. |
| `offset` | Offset | float | `0.5` |  | Added after amplitude. |
| `monochrome` | Monochrome | toggle | `true` |  | One noise value in RGB; off gives three independent channels. |

## Platforms

All platforms.

## Examples

- [Live AI](../../../examples/ai-live.atidarla/) — `noise1`
- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `noise1`
- [Compute Shaders](../../../examples/compute-shaders.atidarla/) — `texture`
- [Image processing](../../../examples/image-processing.atidarla/) — `source`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.atidarla/) — `clouds`
- [Pack: Audio Pulse](../../../examples/pack-audio-pulse.atidarla/) — `clouds`
- [Pack: Colour Grade](../../../examples/pack-colour-grade.atidarla/) — `source`
- [Pack: Duotone](../../../examples/pack-duotone.atidarla/) — `clouds`
- [Pack: Spinning Solid](../../../examples/pack-spinning-solid.atidarla/) — `skin`
- [Pack: Vignette](../../../examples/pack-vignette.atidarla/) — `clouds`
- [Pixel map](../../../examples/pixel-map.atidarla/) — `noise1`
- [Render Textures](../../../examples/render-textures.atidarla/) — `noise`
- [Stage deck](../../../examples/stage-deck.atidarla/) — `tunnel_noise`, `grid_noise`
- [Texture sharing](../../../examples/texture-sharing.atidarla/) — `pattern`
- [Timeline show](../../../examples/timeline-show.atidarla/) — `clouds`
- [Two outputs](../../../examples/two-outputs.atidarla/) — `clouds`
