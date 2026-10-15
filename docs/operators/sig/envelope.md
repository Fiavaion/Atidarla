# Envelope (`sig.envelope`)

SIG · version 1 · cook budget 20 µs

Follows the loudness of an audio input per channel (RMS or peak) with attack and release.

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
| `method` | Method | menu | `"rms"` | `rms`, `peak` | RMS follows perceived loudness; peak reacts to transients. |
| `attack` | Attack | float | `0.01` | ≥ 0 | Seconds to follow a rise. |
| `release` | Release | float | `0.3` | ≥ 0 | Seconds to follow a fall. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
