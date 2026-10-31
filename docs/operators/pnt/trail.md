# Trail (`pnt.trail`)

PNT · version 1 · cook budget 150 µs

The recent history of every point as a line: the last Length positions, newest first, one polyline per point (Render Points draws them with Draw set to lines). Writes `P`, `age` (0 now to 1 oldest), `Cd` (the point's colour, fading out with age when Fade is on) and `id` (the point's number). The history restarts when the point count or Length changes. Particles are followed slot by slot.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `length` | Length | int | `16` | 2 – 64 | Positions kept per point (frames of history). |
| `fade` | Fade | toggle | `true` |  | Multiplies `Cd`'s alpha by 1 − age, so trails fade out. |
| `reset` | Reset | pulse | `false` |  | Restarts every trail at its point's current position. |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `streaks`
