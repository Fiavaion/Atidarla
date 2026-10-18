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

- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `star`
- [Live control](../../../examples/live-control.atidarla/) — `shape1`
- [Pack: Feedback Trails](../../../examples/pack-feedback-trails.atidarla/) — `triangle`
- [Pack: Neon Edges](../../../examples/pack-neon-edges.atidarla/) — `pentagon`
- [Simulations](../../../examples/simulations.atidarla/) — `dot`, `seed`
