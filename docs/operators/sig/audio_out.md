# Audio Out (`sig.audio_out`)

SIG · version 1 · cook budget 20 µs

Plays audio-rate channels (from Audio In, Audio File In or audio processing) on an output device; mono feeds every speaker, otherwise channels map in order. Converts to the device rate; passes the input through.

Sink: cooked every frame even when nothing downstream is viewed, because it has external side effects.

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
| `device` | Device | string | `""` |  | Output device name; empty for the system default. `null` plays nowhere (for testing). |
| `active` | Active | toggle | `true` |  | Open the device and play; off releases it. |
| `gain` | Gain | float | `1.0` | ≥ 0 | Output level (1 = unchanged); samples are clipped to −1…1 at the device. |

## Platforms

All platforms.

## Examples

- [Live control](../../../examples/live-control.atidarla/) — `speaker`
