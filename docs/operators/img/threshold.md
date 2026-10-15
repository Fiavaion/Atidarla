# Threshold (`img.threshold`)

IMG · version 1 · cook budget 80 µs

White where a channel is above the threshold, black elsewhere.

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
| `threshold` | Threshold | float | `0.5` |  | Cut-off value. |
| `softness` | Softness | float | `0.0` | ≥ 0 | Width of the smooth transition around the threshold. |
| `channel` | Channel | menu | `"luminance"` | `luminance`, `red`, `green`, `blue`, `alpha` | Which value is compared. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
