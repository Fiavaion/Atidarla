# Line (`msh.line`)

MSH · version 1 · cook budget 100 µs

A straight polyline between two points, with evenly spaced points (a curve for Sweep).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `start` | Start | vec3 | `[0.0,0.0,0.0]` |  | First point. |
| `end` | End | vec3 | `[0.0,1.0,0.0]` |  | Last point. |
| `points` | Points | int | `2` | 2 – 1000000 | Points along the line, evenly spaced. |

## Platforms

All platforms.

## Examples

- [Mesh Primitives](../../../examples/mesh-primitives.atidarla/) — `horizon`
