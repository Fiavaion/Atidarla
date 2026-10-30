# HSV Adjust (`img.hsv_adjust`)

IMG · version 1 · cook budget 100 µs

Rotates hue and scales saturation and value.

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
| `hue` | Hue Offset | float | `0.0` |  | Hue rotation in degrees. |
| `saturation` | Saturation | float | `1.0` | ≥ 0 | Multiplies saturation. |
| `value` | Value | float | `1.0` | ≥ 0 | Multiplies value (brightness). |

## Platforms

All platforms.

## Examples

- [Beat clock](../../../examples/beat-clock.atidarla/) — `colour`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.atidarla/) — `colour`
- [Pack: Audio Pulse](../../../examples/pack-audio-pulse.atidarla/) — `colour`
- [Pack: LFO Bank](../../../examples/pack-lfo-bank.atidarla/) — `hue`
- [Texture sharing](../../../examples/texture-sharing.atidarla/) — `tint`
