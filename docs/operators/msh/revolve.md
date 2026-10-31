# Revolve (`msh.revolve`)

MSH · version 1 · cook budget 100 µs

Turns every primitive of the input about an axis (a lathe): a full turn or an arc, with end caps for closed profiles. Points on the axis are welded. Writes `N` and `uv`.

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
| `origin` | Origin | vec3 | `[0.0,0.0,0.0]` |  | A point on the axis. |
| `axis` | Axis | vec3 | `[0.0,1.0,0.0]` |  | The direction of the axis turned about. |
| `start` | Start Angle | float | `0.0` |  | Degrees where the surface starts. |
| `end` | End Angle | float | `360.0` |  | Degrees where it ends; a full turn (360 from the start) closes the surface. |
| `divisions` | Divisions | int | `32` | 1 – 4096 | Steps around the axis. |
| `caps` | Caps | toggle | `true` |  | Closes both ends of a partial turn with the profile, when the profile is a closed polygon. |
| `reverse` | Reverse | toggle | `false` |  | Flips the surface (a profile drawn downward faces inward). |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.atidarla/) — `cup`
