# Onset (`sig.onset`)

SIG · version 1 · cook budget 200 µs

Detects onsets (beats, hits) in an audio input by spectral flux. Outputs `onset` (1 on detection frames) and `flux`.

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
| `fft_size` | FFT Size | menu | `"1024"` | `512`, `1024`, `2048`, `4096` | Analysis window in samples. |
| `sensitivity` | Sensitivity | float | `1.5` | ≥ 1 | How far above the recent average the flux must rise (lower = more onsets). |
| `floor` | Floor | float | `0.01` | ≥ 0 | Minimum flux to count, ignoring near-silence. |
| `min_interval` | Min Interval | float | `0.1` | ≥ 0 | Seconds between onsets. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
