# Flock (`pnt.flock`)

PNT · version 1 · cook budget 900 µs

Boids: steers each point's velocity `v` (created at zero) by its neighbours within Radius: Separation pushes away from those closer than Personal Space, Alignment turns toward their average velocity, Cohesion toward their centre; the speed is then held between Min Speed and Max Speed. Forces act over the frame's step, like the other forces: put it with Integrate in a particle or Solver loop. Neighbours come from a GPU grid hash; every point reads the previous frame's velocities, so the result repeats exactly.

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
| `radius` | Radius | float | `0.3` | ≥ 0.000001 | How far a point sees its neighbours (also the grid's cell size). |
| `max_neighbours` | Max Neighbours | int | `32` | 1 – 256 | At most this many neighbours are used per point (in a fixed order). |
| `separation` | Separation | float | `2.0` |  | Strength of the push away from neighbours closer than Personal Space. |
| `personal_space` | Personal Space | float | `0.1` | ≥ 0 | Neighbours closer than this push the point away. |
| `alignment` | Alignment | float | `1.0` |  | Strength of the turn toward the neighbours' average velocity. |
| `cohesion` | Cohesion | float | `1.0` |  | Strength of the pull toward the neighbours' centre. |
| `min_speed` | Min Speed | float | `0.2` | ≥ 0 | Slower points are sped up to this (along their heading, or +X when still). |
| `max_speed` | Max Speed | float | `1.0` | ≥ 0 | Faster points are slowed to this. |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.lasair/) — `boids`
