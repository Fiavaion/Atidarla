# To Channels (`pnt.to_channels`)

PNT · version 1 · cook budget 60 µs

Reads one point attribute back from the GPU as channels: one channel per point and component (`P0_x`, `P0_y`, …), or one channel per component with a sample per point. At most `max` points. Values trail the points by a frame or two live (GPU readback).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `attr` | Attribute | string | `"P"` |  | The point attribute to read (`P`, `Cd`, `v`, `age`, any float or integer attribute). |
| `layout` | Layout | menu | `"points"` | `points`, `samples` | `points`: one channel per point and component, `<attr><i>_<c>` (`P0_x`; `age3` for one component), at control rate. `samples`: one channel per component, `<attr>_<c>` (`P_x`; `age`), each with one sample per point in point order, like a Pattern. Components are `x y z w`, or `r g b a` for `Cd`. |
| `max` | Max Points | int | `64` | 1 – 4096 | Reads the first `max` points (of a simulation, the first `max` live ones). Caps the channel count at `max` × 4. |

## Platforms

All platforms.

## Examples

- [PNT Outputs](../../../examples/pnt-outputs.atidarla/) — `positions`
