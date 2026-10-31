# Shape (`img.shape`)

IMG · version 1 · cook budget 80 µs

A circle, rectangle or regular polygon with soft edges.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `shape` | Shape | menu | `"circle"` | `circle`, `rectangle`, `polygon` | Which shape to draw. |
| `centre` | Centre | vec2 | `[0.5,0.5]` |  | Centre as a fraction of the image from the bottom-left. |
| `size` | Size | vec2 | `[0.5,0.5]` |  | Width and height as fractions of the image height (so circles stay round). |
| `sides` | Sides | int | `6` | 3 – 64 | Polygon sides. |
| `rotate` | Rotate | float | `0.0` |  | Rotation in degrees, counter-clockwise. |
| `softness` | Softness | float | `0.005` | ≥ 0 | Edge blur as a fraction of the image height. |
| `fill` | Fill | colour | `[1.0,1.0,1.0,1.0]` |  | Colour inside the shape. |
| `background` | Background | colour | `[0.0,0.0,0.0,0.0]` |  | Colour outside the shape. |

## Platforms

All platforms.

## Examples

- [Beat clock](../../../examples/beat-clock.atidarla/) — `star`
- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `star`
- [Compute Shaders](../../../examples/compute-shaders.atidarla/) — `dot`
- [Live control](../../../examples/live-control.atidarla/) — `shape1`
- [Neural Growth](../../../examples/neural-growth.atidarla/) — `dot`
- [Pack: Colour Grade](../../../examples/pack-colour-grade.atidarla/) — `left_half`
- [Pack: Feedback Trails](../../../examples/pack-feedback-trails.atidarla/) — `triangle`
- [Pack: Film Grain](../../../examples/pack-film-grain.atidarla/) — `sun`
- [Pack: LFO Bank](../../../examples/pack-lfo-bank.atidarla/) — `pentagon`
- [Pack: Neon Edges](../../../examples/pack-neon-edges.atidarla/) — `pentagon`
- [Pack: RGB Delay](../../../examples/pack-rgb-delay.atidarla/) — `bar`, `triangle`
- [Simulations](../../../examples/simulations.atidarla/) — `dot`, `seed`
- [Soak show](../../../examples/soak-show.atidarla/) — `circle`, `star`, `bar`, `dots`, `burst`, `frame`
- [Stage deck](../../../examples/stage-deck.atidarla/) — `circle`, `star`, `bar`, `dots`, `burst`, `frame`
- [Two outputs](../../../examples/two-outputs.atidarla/) — `sun`
