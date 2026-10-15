# Filter (`sig.filter`)

SIG · version 1 · cook budget 20 µs

One-pole low-pass (smooth) or high-pass (changes only) filter per channel.

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
| `type` | Type | menu | `"lowpass"` | `lowpass`, `highpass` | Low-pass keeps slow movement; high-pass keeps fast changes. |
| `cutoff` | Cutoff | float | `2.0` | ≥ 0 | Cut-off frequency in Hz. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
