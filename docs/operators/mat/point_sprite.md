# Point Sprite (`mat.point_sprite`)

MAT · version 1 · cook budget 20 µs

Draws the geometry's points (not its polygons) as discs or squares facing the camera, sized in world units (scaled by `pscale` when present, and by perspective) or in pixels; optionally shaded like small spheres by the renderer's light.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Sprite colour. |
| `size_mode` | Size Mode | menu | `"world"` | `world`, `pixels` | world: Size is in world units, so far sprites look smaller; pixels: Size is in pixels. |
| `size` | Size | float | `0.05` | ≥ 0 | Diameter in world units or pixels (see Size Mode). |
| `shape` | Shape | menu | `"disc"` | `disc`, `square` | disc (antialiased edge) or square. |
| `sphere` | Sphere Shading | toggle | `false` |  | Shade each disc as a small sphere lit by the renderer's light. |
| `use_cd` | Use Cd | toggle | `true` |  | Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one. |

## Platforms

All platforms.

## Examples

- [Render Wire and Toon](../../../examples/render-wire-toon.lasair/) — `beads`
