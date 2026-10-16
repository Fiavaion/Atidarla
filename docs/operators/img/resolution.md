# Resolution (`img.resolution`)

IMG · version 1 · cook budget 40 µs

Resamples the input to another size: a fraction of the input (or of the project size when nothing is connected), or exact pixels. Operators after it work at the new size.

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
| `mode` | Mode | menu | `"fraction"` | `fraction`, `pixels` | `fraction` scales the input size; `pixels` sets width and height. |
| `fraction` | Fraction | float | `0.5` | 0.01 – 8 | Scale applied to both sides in `fraction` mode. |
| `width` | Width | int | `1280` | 1 – 16384 | Output width in `pixels` mode. |
| `height` | Height | int | `720` | 1 – 16384 | Output height in `pixels` mode. |

## Platforms

All platforms.

## Examples

- [Keying](../../../examples/keying.atidarla/) — `half`
