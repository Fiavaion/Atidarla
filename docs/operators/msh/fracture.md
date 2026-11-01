# Fracture (`msh.fracture`)

MSH · version 1 · cook budget 100 µs

Breaks a closed mesh into Voronoi pieces around seeded random points inside it (or the `points` input), with an optional gap. Writes `piece` per point and primitive; cut faces are the group `inside`, the original surface `outside`. Heavy: usually baked.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `points` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `count` | Pieces | int | `12` | 1 – 512 | Without a `points` input: the number of seeded random seed points inside the mesh. |
| `seed` | Seed | int | `1` | ≥ 0 | Random seed for the seed points: the same seed gives the same pieces. |
| `gap` | Gap | float | `0.0` | ≥ 0 | Space left between neighbouring pieces (each cut moves back by half of it). |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.lasair/) — `pieces`
