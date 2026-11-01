# Channel Image (`img.channel_image`)

IMG · version 1 · cook budget 80 µs

Draws SIG channels as an image: waveforms (samples along x, a row per channel) or band rows (channels along x). Feeds ISF audio inputs.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `sig` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `layout` | Layout | menu | `"samples"` | `samples`, `channels` | `samples`: samples along x, one row per channel (channel 0 at the bottom). `channels`: channels along x, one row per sample. |
| `scale` | Scale | float | `1.0` |  | Multiplies each value before it is written. |
| `offset` | Offset | float | `0.0` |  | Added after scaling; 0.5 with scale 0.5 maps a −1…1 waveform to 0…1. |
| `interpolate` | Interpolate | toggle | `true` |  | Blend linearly between neighbouring values; off shows hard steps. |

## Platforms

All platforms.

## Examples

- [Signal scope](../../../examples/signal-scope.lasair/) — `waveform`, `trace`, `bars`, `others`
- [Speech and chat](../../../examples/speech-chat.lasair/) — `bars`
