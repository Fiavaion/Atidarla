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

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.lasair/) — `level1`
- [Beat clock](../../../examples/beat-clock.lasair/) — `beat_flash`
- [Beat switcher](../../../examples/beat-switcher.lasair/) — `level1`
- [Live control](../../../examples/live-control.lasair/) — `level1`
- [Neural Growth](../../../examples/neural-growth.lasair/) — `dim`
- [Python operators](../../../examples/python-ops.lasair/) — `level1`
- [Signal scope](../../../examples/signal-scope.lasair/) — `meter`
- [Soak show](../../../examples/soak-show.lasair/) — `trail`
- [Timeline show](../../../examples/timeline-show.lasair/) — `level1`
