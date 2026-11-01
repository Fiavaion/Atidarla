# Tile (`img.tile`)

IMG · version 1 · cook budget 60 µs

Repeats the input in a grid.

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
| `repeat` | Repeat | vec2 | `[2.0,2.0]` |  | Number of tiles across and down. |
| `mirror` | Mirror | toggle | `false` |  | Mirrors every other tile for seamless edges. |

## Platforms

All platforms.

## Examples

- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.lasair/) — `tile1`
