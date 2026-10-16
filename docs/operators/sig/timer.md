# Timer (`sig.timer`)

SIG · version 1 · cook budget 20 µs

Counts seconds while playing: `seconds`, `fraction` of the length and completed `cycles`.

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
| `length` | Length | float | `4.0` | ≥ 0 | Cycle length in seconds. |
| `loop` | Loop | toggle | `true` |  | Wrap around at the end instead of holding. |
| `play` | Play | toggle | `true` |  | Advance the timer. |
| `reset` | Reset | toggle | `false` |  | While on, holds the timer at zero. |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `clock1`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.atidarla/) — `clock`
