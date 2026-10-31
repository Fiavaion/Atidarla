# Mirror (`img.mirror`)

IMG · version 1 · cook budget 60 µs

Reflects one half of the image onto the other.

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
| `axis` | Axis | menu | `"left_to_right"` | `left_to_right`, `top_to_bottom`, `both` | Which half is kept and reflected; `both` mirrors in both directions. |

## Platforms

All platforms.

## Examples

- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.atidarla/) — `mirror1`
- [Soak show](../../../examples/soak-show.atidarla/) — `bars`
- [Stage deck](../../../examples/stage-deck.atidarla/) — `bars`
