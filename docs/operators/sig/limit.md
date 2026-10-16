# Limit (`sig.limit`)

SIG · version 1 · cook budget 20 µs

Clamps channels to a range, optionally quantising to a step.

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
| `min` | Min | float | `0.0` |  | Lowest output value. |
| `max` | Max | float | `1.0` |  | Highest output value. |
| `step` | Step | float | `0.0` | ≥ 0 | Quantisation step; 0 disables it. |

## Platforms

All platforms.

## Examples

- [Signal scope](../../../examples/signal-scope.atidarla/) — `clamped`
