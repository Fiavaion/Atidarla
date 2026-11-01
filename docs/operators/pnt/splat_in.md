# Splat In (`pnt.splat_in`)

PNT · version 1 · cook budget 50 µs

Reads Gaussian splats (a 3D Gaussian Splatting capture) from a .ply in the standard 3DGS layout, an .spz, or a .sog archive or SOG folder (its meta.json), on a worker thread. Each splat is a point with `P`, `Cd` (base colour, opacity in alpha), `scale` (per-axis size), `orient` (rotation) and, up to degree 3, the spherical-harmonic coefficients `sh0`… that give view-dependent colour. Draw it with Splat MAT in Render.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `file` | File | path | `""` |  | A .ply, .spz or .sog file, or a SOG folder's meta.json (relative paths are relative to the project folder). |
| `y_down` | Y Down Source | toggle | `true` |  | PLY and SOG captures are stored in the trainer's camera frame (Y down, Z forward): turn them a half turn about X so they stand upright here. SPZ files are stored Y up already and are never turned. Off: PLY and SOG coordinates as stored. |

## Platforms

All platforms.

## Examples

- [Gaussian Splats](../../../examples/splats.lasair/) — `ply_in`, `spz_in`, `sog_in`
