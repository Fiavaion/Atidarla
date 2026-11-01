# LFO (`sig.lfo`)

SIG · version 1 · cook budget 20 µs

A low-frequency oscillator: sine, triangle, square or ramp over time, or over the beats of a clock. Outputs channel `lfo`.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `clock` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `waveform` | Waveform | menu | `"sine"` | `sine`, `triangle`, `square`, `ramp` | Shape of one cycle. |
| `frequency` | Frequency | float | `1.0` |  | Cycles per second; with Sync on beats, cycles per beat (0.25 = one cycle per bar of four). |
| `amplitude` | Amplitude | float | `1.0` |  | Output = offset + amplitude × wave. |
| `offset` | Offset | float | `0.0` |  | Added to the scaled wave. |
| `phase` | Phase | float | `0.0` |  | Shift in cycles (0–1). |
| `sync` | Sync | menu | `"off"` | `off`, `beats` | `beats` locks the wave to the beats of the clock input (a Beat operator), so it stays in step through tempo changes, taps and resyncs. |

## Platforms

All platforms.

## Examples

- [Beat clock](../../../examples/beat-clock.lasair/) — `sweep`, `flash`
- [Compute Shaders](../../../examples/compute-shaders.lasair/) — `drift`
- [DAT tables](../../../examples/dat-tables.lasair/) — `lfo`
- [Image processing](../../../examples/image-processing.lasair/) — `move`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.lasair/) — `hue_swing`
- [Neural Growth](../../../examples/neural-growth.lasair/) — `sweep`
- [Pack: Chromatic Aberration](../../../examples/pack-chromatic-aberration.lasair/) — `wobble`
- [Pack: Feedback Trails](../../../examples/pack-feedback-trails.lasair/) — `swing`
- [Pack: Neon Edges](../../../examples/pack-neon-edges.lasair/) — `colours`
- [Pack: Spark Fountain](../../../examples/pack-spark-fountain.lasair/) — `breath`
- [Simulations](../../../examples/simulations.lasair/) — `orbit`
- [Soak show](../../../examples/soak-show.lasair/) — `drift`, `pick`
- [Stage deck](../../../examples/stage-deck.lasair/) — `drift`, `pick`
- [Texture sharing](../../../examples/texture-sharing.lasair/) — `drift`
- [Timeline show](../../../examples/timeline-show.lasair/) — `pulse`
- [Two outputs](../../../examples/two-outputs.lasair/) — `drift`
