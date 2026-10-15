# Movie In (`img.movie_in`)

IMG · version 1 · cook budget 300 µs

Plays a movie file. HAP, HAP Alpha, HAP Q and HAP R are decoded natively and upload as GPU-compressed textures; other codecs arrive with FFmpeg.

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
| `file` | File | string | `""` |  | Path to a `.mov` file (HAP family). |
| `play` | Play | toggle | `true` |  | Advance the playhead; off holds the frame at Cue. |
| `loop` | Loop | toggle | `true` |  | Restart at the end; off holds the last frame. |
| `speed` | Speed | float | `1.0` |  | Playback rate; negative plays backwards. |
| `cue` | Cue | float | `0.0` | ≥ 0 | Position in seconds while stopped; playback starts from here. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
