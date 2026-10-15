# Edge (`img.edge`)

IMG · version 1 · cook budget 120 µs

Outlines: the strength of luminance edges (Sobel).

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
| `strength` | Strength | float | `1.0` | ≥ 0 | Multiplies the edge strength. |
| `mode` | Mode | menu | `"magnitude"` | `magnitude`, `colour` | Grey edges, or the input colour masked by edges. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
