# Feedback (`img.feedback`)

IMG · version 1 · cook budget 40 µs

Outputs the previous frame of the `loop` input, closing a feedback loop; outputs `in` until the loop has a frame or while reset is on.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |
| `loop` | Img | feedback: reads the source's previous frame; optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `reset` | Reset | toggle | `false` |  | While on, outputs `in` and restarts the loop. |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `feedback1`
- [Soak show](../../../examples/soak-show.atidarla/) — `feedback`
