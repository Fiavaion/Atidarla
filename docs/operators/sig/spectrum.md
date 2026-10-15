# Spectrum (`sig.spectrum`)

SIG · version 1 · cook budget 200 µs

Band energies of an audio input (log-spaced `band1`…`bandN`, a full-scale sine reads about 1) plus its RMS `level`.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `fft_size` | FFT Size | menu | `"1024"` | `512`, `1024`, `2048`, `4096` | Analysis window in samples; larger resolves lower frequencies but reacts more slowly. |
| `bands` | Bands | int | `8` | 1 – 32 | Number of log-spaced bands. |
| `min_freq` | Min Frequency | float | `30.0` | ≥ 1 | Lower edge of the first band in Hz. |
| `max_freq` | Max Frequency | float | `16000.0` | ≥ 1 | Upper edge of the last band in Hz. |
| `smoothing` | Smoothing | float | `0.5` | 0 – 0.99 | Per-frame smoothing of band values (0 = none). |
| `gain` | Gain | float | `1.0` | ≥ 0 | Scales the band and level outputs. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
