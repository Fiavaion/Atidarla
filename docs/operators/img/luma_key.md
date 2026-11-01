# Luma Key (`img.luma_key`)

IMG · version 1 · cook budget 80 µs

Makes pixels outside a luminance range transparent.

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
| `low` | Low | float | `0.0` |  | Lowest luminance kept. |
| `high` | High | float | `0.5` |  | Highest luminance kept; set Low above it to key out the range instead. |
| `softness` | Softness | float | `0.05` | ≥ 0 | Fade width at both edges. |

## Platforms

All platforms.

## Examples

- [Keying](../../../examples/keying.lasair/) — `bright`
