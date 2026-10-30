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

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `level1`
- [Beat clock](../../../examples/beat-clock.atidarla/) — `beat_flash`
- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `level1`
- [Live control](../../../examples/live-control.atidarla/) — `level1`
- [Neural Growth](../../../examples/neural-growth.atidarla/) — `dim`
- [Python operators](../../../examples/python-ops.atidarla/) — `level1`
- [Signal scope](../../../examples/signal-scope.atidarla/) — `meter`
