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

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
