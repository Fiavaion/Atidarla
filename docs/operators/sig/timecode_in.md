# Timecode In (`sig.timecode_in`)

SIG · version 1 · cook budget 20 µs

SMPTE timecode from MIDI Time Code or from LTC audio. `time` is the position in seconds and keeps moving between updates; `locked` is 1 while code is arriving. The input runs on its own thread.

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
| `source` | Source | menu | `"mtc"` | `mtc`, `ltc` | `mtc` (MIDI Time Code from a MIDI port) or `ltc` (Linear Timecode on an audio input). |
| `midi_device` | MIDI Device | string | `""` |  | MTC: part of the MIDI input's name; empty for the first port; `virtual` for no device. |
| `audio_device` | Audio Device | string | `""` |  | LTC: audio input name; empty for the system default. |
| `audio_channel` | Audio Channel | int | `1` | 1 – 32 | LTC: which input channel carries the code. |
| `offset` | Offset | float | `0.0` |  | Seconds added to `time`. |

## Platforms

All platforms.

## Examples

- [Live control](../../../examples/live-control.lasair/) — `timecode1`
