# Movie In (`img.movie_in`)

IMG · version 1 · cook budget 300 µs

Plays a movie or still image. HAP, HAP Alpha, HAP Q and HAP R are decoded natively and upload as GPU-compressed textures; PNG and JPEG stills load as one frame; other codecs (H.264, HEVC, ProRes, VP9 and more) decode through FFmpeg, with hardware decoding where the platform has it, in builds with FFmpeg.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `file` | File | path | `""` |  | Path to a `.mov` file (HAP family) or a PNG/JPEG still. |
| `play` | Play | toggle | `true` |  | Advance the playhead; off holds the frame at Cue. |
| `loop` | Loop | toggle | `true` |  | Restart at the end; off holds the last frame. |
| `speed` | Speed | float | `1.0` |  | Playback rate; negative plays backwards. |
| `cue` | Cue | float | `0.0` | ≥ 0 | Position in seconds while stopped; playback starts from here. |
| `size` | Size | menu | `"native"` | `native`, `project` | `native` outputs at the file's resolution; `project` at the project resolution. |

## Platforms

All platforms.

## Examples

- [Keying](../../../examples/keying.lasair/) — `footage`
- [Time effects](../../../examples/time-effects.lasair/) — `clip`
