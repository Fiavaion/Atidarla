# Level (`img.level`)

IMG · version 1 · cook budget 100 µs

Brightness, contrast, gamma, opacity and invert.

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
| `brightness` | Brightness | float | `1.0` | ≥ 0 | Multiplies RGB. |
| `contrast` | Contrast | float | `1.0` | ≥ 0 | Scales RGB around mid-grey (0.5). |
| `gamma` | Gamma | float | `1.0` | ≥ 0.01 | Applies RGB^(1/gamma). |
| `opacity` | Opacity | float | `1.0` | 0 – 1 | Multiplies alpha. |
| `invert` | Invert | toggle | `false` |  | Inverts RGB before the other adjustments. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
