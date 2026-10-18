# UV Project (`msh.uv_project`)

MSH · version 1 · cook budget 100 µs

Writes texture coordinates `uv` per point by a planar, cylindrical, spherical or box projection, fitted to the bounds.

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
| `projection` | Projection | menu | `"planar"` | `planar`, `cylindrical`, `spherical`, `box` | planar: straight along the axis; cylindrical: around the axis (u) and along it (v); spherical: longitude and latitude around the centre; box: planar along whichever axis each point's normal faces most. |
| `axis` | Axis | menu | `"z"` | `x`, `y`, `z` | planar: the projection direction; cylindrical and spherical: the axis (the poles). |
| `fit` | Fit to Bounds | toggle | `true` |  | Scales the projection so the geometry's bounds span 0 to 1 (else world units). |
| `scale` | Scale | vec2 | `[1.0,1.0]` |  | Multiplies u and v. |
| `offset` | Offset | vec2 | `[0.0,0.0]` |  | Added to u and v after scaling. |

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.atidarla/) — `wrap`
