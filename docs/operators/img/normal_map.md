# Normal Map (`img.normal_map`)

IMG · version 1 · cook budget 120 µs

Treats luminance as height and outputs tangent-space normals (RGB = XYZ × 0.5 + 0.5).

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
| `strength` | Strength | float | `2.0` | ≥ 0 | Height scale; higher gives steeper normals. |

## Platforms

All platforms.

## Examples

- [Image processing](../../../examples/image-processing.lasair/) — `normals`
