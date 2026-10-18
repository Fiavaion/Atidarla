# Transform (`img.transform`)

IMG · version 1 · cook budget 80 µs

Translates, rotates and scales the input around a pivot.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `translate` | Translate | vec2 | `[0.0,0.0]` |  | Offset as a fraction of the image; +y is up. |
| `rotate` | Rotate | float | `0.0` |  | Rotation in degrees, counter-clockwise. |
| `scale` | Scale | vec2 | `[1.0,1.0]` |  | Scale factors (must not be zero). |
| `pivot` | Pivot | vec2 | `[0.5,0.5]` |  | Centre of rotation and scale, as a fraction of the image from the bottom-left. |
| `extend` | Extend | menu | `"zero"` | `zero`, `hold`, `repeat`, `mirror` | What appears outside the input: transparent, edge pixels, repeats or mirrored repeats. |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `transform1`
- [Compute Shaders](../../../examples/compute-shaders.atidarla/) — `orbit`
- [Neural Growth](../../../examples/neural-growth.atidarla/) — `eraser`
- [Pack: Beat Flash](../../../examples/pack-beat-flash.atidarla/) — `kick`
- [Pack: Feedback Trails](../../../examples/pack-feedback-trails.atidarla/) — `orbit`
- [Simulations](../../../examples/simulations.atidarla/) — `orbiting`
