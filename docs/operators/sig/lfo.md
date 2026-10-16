# LFO (`sig.lfo`)

SIG · version 1 · cook budget 20 µs

A low-frequency oscillator: sine, triangle, square or ramp over time. Outputs channel `lfo`.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `waveform` | Waveform | menu | `"sine"` | `sine`, `triangle`, `square`, `ramp` | Shape of one cycle. |
| `frequency` | Frequency | float | `1.0` |  | Cycles per second. |
| `amplitude` | Amplitude | float | `1.0` |  | Output = offset + amplitude × wave. |
| `offset` | Offset | float | `0.0` |  | Added to the scaled wave. |
| `phase` | Phase | float | `0.0` |  | Shift in cycles (0–1). |

## Platforms

All platforms.

## Examples

- [Image processing](../../../examples/image-processing.atidarla/) — `move`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.atidarla/) — `hue_swing`
- [Simulations](../../../examples/simulations.atidarla/) — `orbit`
