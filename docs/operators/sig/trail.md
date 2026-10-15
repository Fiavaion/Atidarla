# Trail (`sig.trail`)

SIG · version 1 · cook budget 20 µs

Keeps the recent history of each channel: each output channel holds the last N frames, oldest first.

Cooks every frame (time-dependent).

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
| `length` | Length | int | `120` | 2 – 4096 | Frames of history. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
