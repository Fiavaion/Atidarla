# Lag (`sig.lag`)

SIG · version 1 · cook budget 20 µs

Smooths changes with separate rise and fall times.

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
| `lag_up` | Lag Up | float | `0.2` | ≥ 0 | Time constant in seconds when rising. |
| `lag_down` | Lag Down | float | `0.2` | ≥ 0 | Time constant in seconds when falling. |

## Platforms

All platforms.

## Examples

- [Signal scope](../../../examples/signal-scope.lasair/) — `slow`
