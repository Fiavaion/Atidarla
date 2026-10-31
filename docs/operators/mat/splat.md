# Splat (`mat.splat`)

MAT · version 1 · cook budget 20 µs

Draws the geometry's points as Gaussian splats (3D Gaussian Splatting), as Splat In reads them: each an ellipsoid from `scale` and `orient`, coloured by `Cd` and, up to SH Degree, the view-dependent spherical harmonics `sh0`…, sorted on the GPU every frame and blended back to front over the render's other geometry. Points without `scale` are drawn as round splats.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `tint` | Tint | colour | `[1.0,1.0,1.0,1.0]` |  | Multiplies every splat's colour (alpha: its opacity). |
| `opacity` | Opacity | float | `1.0` | 0 – 4 | Multiplies every splat's opacity (above 1 thickens a sparse capture). |
| `size` | Size | float | `1.0` | 0 – 16 | Multiplies every splat's size (below 1 shows the capture's structure). |
| `sh_degree` | SH Degree | int | `3` | 0 – 3 | Highest spherical-harmonic band used for view-dependent colour (up to what the geometry has); 0: the base colour only, the fastest. |

## Platforms

All platforms.

## Examples

- [Gaussian Splats](../../../examples/splats.atidarla/) — `splats_ply`, `splats_spz`, `splats_sog`
