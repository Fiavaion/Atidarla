# Trigger (`sig.trigger`)

SIG · version 1 · cook budget 20 µs

An attack/decay envelope fired when the input rises above a threshold or Fire is switched on. Outputs `trigger`.

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
| `threshold` | Threshold | float | `0.5` |  | Input value that counts as high. |
| `attack` | Attack | float | `0.01` | ≥ 0 | Seconds to rise to 1. |
| `decay` | Decay | float | `0.5` | ≥ 0 | Seconds to fall back to 0. |
| `fire` | Fire | toggle | `false` |  | Turning this on fires the trigger. |

## Platforms

All platforms.

## Examples

- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `kick`
