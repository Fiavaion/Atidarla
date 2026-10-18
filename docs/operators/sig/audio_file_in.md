# Audio File In (`sig.audio_file_in`)

SIG · version 1 · cook budget 20 µs

Plays a WAV file as audio-rate channels (`left`/`right`, or `mono`). Decoding runs off the render thread.

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
| `file` | File | path | `""` |  | Path to a WAV file (PCM or float). |
| `play` | Play | toggle | `true` |  | Advance the playhead. |
| `loop` | Loop | toggle | `true` |  | Restart at the end of the file. |
| `gain` | Gain | float | `1.0` | ≥ 0 | Scales the samples. |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `audio1`
- [Live control](../../../examples/live-control.atidarla/) — `drums`
- [Pack: Audio Pulse](../../../examples/pack-audio-pulse.atidarla/) — `drums`
