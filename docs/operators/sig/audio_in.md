# Audio In (`sig.audio_in`)

SIG · version 1 · cook budget 20 µs

Live audio from an input device as audio-rate channels (`left`/`right`, `mono`, or `chan1`…). The device opens and runs on its own thread.

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
| `device` | Device | string | `""` |  | Input device name; empty for the system default. `test-tone` is a built-in 440/660 Hz stereo source. |
| `active` | Active | toggle | `true` |  | Capture from the device; off releases it. |
| `gain` | Gain | float | `1.0` | ≥ 0 | Scales the samples. |

## Platforms

All platforms.

## Examples

- [Signal scope](../../../examples/signal-scope.atidarla/) — `mic`
