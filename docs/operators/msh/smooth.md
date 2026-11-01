# Smooth (`msh.smooth`)

MSH · version 1 · cook budget 100 µs

Smooths the surface without changing its topology: Taubin smoothing (keeps the size) or Laplacian smoothing (shrinks), optionally keeping open borders and limited to a point group.

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
| `method` | Method | menu | `"taubin"` | `taubin`, `laplacian` | taubin: alternates smoothing and inflating steps, so noise goes but the shape keeps its size; laplacian: moves each point towards the average of its neighbours (shrinks the shape as it smooths). |
| `iterations` | Iterations | int | `10` | 0 – 1000 | Smoothing steps (taubin: each step is one smoothing and one inflating pass). |
| `strength` | Strength | float | `0.5` | 0 – 1 | Share of the way each point moves towards the average of its neighbours per step. |
| `pass_band` | Pass Band | float | `0.1` | 0.01 – 0.5 | taubin: the frequency below which shapes are kept (smaller keeps only broader shapes, smoothing more). |
| `keep_boundary` | Keep Boundary | toggle | `true` |  | Points on open borders (and the ends of polylines) stay put; off, border points are smoothed along the border only. |
| `group` | Group | string | `""` |  | Only the points of this point group move (empty: all points). |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.lasair/) — `calm`
