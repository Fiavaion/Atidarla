# Cache (`img.cache`)

IMG · version 1 · cook budget 60 µs

Delays the input by a number of frames (frame history).

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `delay` | Delay | int | `10` | 0 – 120 | Frames of delay; 0 passes the input through. |

## Platforms

All platforms.

## Examples

- [Time effects](../../../examples/time-effects.atidarla/) — `delayed`
