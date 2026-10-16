# Lookup (`img.lookup`)

IMG · version 1 · cook budget 80 µs

Recolours input A through a lookup image B: each pixel's luminance picks a colour along B's horizontal centre line.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |
| `lut` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `offset` | Offset | float | `0.0` |  | Shifts the lookup position (wraps). |

## Platforms

All platforms.

## Examples

- [Image processing](../../../examples/image-processing.atidarla/) — `coloured`
