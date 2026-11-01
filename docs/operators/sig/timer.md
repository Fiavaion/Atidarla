# Timer (`sig.timer`)

SIG · version 1 · cook budget 20 µs

Counts seconds while playing: `seconds`, `fraction` of the length and completed `cycles`. Synced, it counts the beats of a clock instead.

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
| `length` | Length | float | `4.0` | ≥ 0 | Cycle length in seconds; with Sync on beats, in beats (4 = one bar of four). |
| `loop` | Loop | toggle | `true` |  | Wrap around at the end instead of holding. |
| `play` | Play | toggle | `true` |  | Advance the timer. |
| `reset` | Reset | toggle | `false` |  | While on, holds the timer at zero. |
| `sync` | Sync | menu | `"off"` | `off`, `beats` | `beats` counts the beats of the clock input (a Beat operator) instead of seconds, so cycles stay in step with the bar; the `seconds` channel then reads in beats. Reset restarts the count at the current beat. |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.lasair/) — `clock1`
- [Beat clock](../../../examples/beat-clock.lasair/) — `cycle`
- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.lasair/) — `clock`
