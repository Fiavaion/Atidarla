# Collide (`pnt.collide`)

PNT · version 1 · cook budget 60 µs

Keeps points out of a plane or a sphere (or inside a sphere): a point that has passed the surface is moved back onto it and its velocity `v` is reflected, scaled by Bounce, with Friction taking off sliding speed. Writes `P` and `v` (created at zero when missing).

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
| `shape` | Shape | menu | `"plane"` | `plane`, `sphere_outside`, `sphere_inside` | plane: the side the normal points to is free; sphere_outside: points stay outside the ball; sphere_inside: points stay inside it. |
| `centre` | Centre | vec3 | `[0.0,-1.0,0.0]` |  | A point on the plane, or the sphere's centre. |
| `normal` | Normal | vec3 | `[0.0,1.0,0.0]` |  | The plane's normal (toward the free side). |
| `radius` | Radius | float | `1.0` | ≥ 0 | The sphere's radius. |
| `bounce` | Bounce | float | `0.5` | ≥ 0 | Fraction of the normal speed kept after a hit (0: no bounce, 1: elastic). |
| `friction` | Friction | float | `0.1` | 0 – 1 | Fraction of the sliding speed lost at a hit. |

## Platforms

All platforms.

## Examples

No example project uses it yet.
