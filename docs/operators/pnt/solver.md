# Solver (`pnt.solver`)

PNT · version 1 · cook budget 20 µs

The entry of a simulation loop: outputs `init` at the start (first frame, Reset, until Start Frame, or while `prev` has no geometry), then the previous frame's state from `prev`. Wire the end of the body (for example Gravity → Collide → Integrate → Age) into `prev`; the body may sit in a Pack (a Solver Block). The state is updated in place when nothing else reads it.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `init` | Geo | optional |
| `prev` | Geo | feedback: reads the source's previous frame; optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `reset` | Reset | toggle | `false` |  | While on, outputs `init` every frame (the simulation restarts when it is turned off). |
| `start_frame` | Start Frame | int | `0` | ≥ 0 | Up to this frame the output is `init`; the simulation runs from the next. |

## Platforms

All platforms.

## Examples

- [PNT Solver](../../../examples/pnt-solver.atidarla/) — `sim`
