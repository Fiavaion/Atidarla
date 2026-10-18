# Platonic Solid (`msh.platonic`)

MSH · version 1 · cook budget 100 µs

A tetrahedron, cube, octahedron, dodecahedron or icosahedron centred on Centre, or a flat regular polygon facing +Z, as polygons wound counter-clockwise seen from outside. Each face has its own points, so it writes flat normals `N` and per-face texture coordinates `uv` (the polygon's span the unit square); optionally a colour per face (prim `Cd`).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `shape` | Shape | menu | `"icosahedron"` | `tetrahedron`, `cube`, `octahedron`, `dodecahedron`, `icosahedron`, `polygon` | Which solid, or polygon: one flat face with Sides corners. |
| `radius` | Radius | float | `1.0` | ≥ 0 | Distance from the centre to every corner. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Where the shape is centred. |
| `sides` | Sides | int | `4` | 3 – 256 | Polygon: number of corners (4 is a square, edges along the axes). |
| `face_colours` | Face Colours | toggle | `false` |  | Give every face its own colour (prim `Cd`), evenly spread around the hue circle. |

## Platforms

All platforms.

## Examples

- [Render Solids](../../../examples/render-solids.atidarla/) — `dodeca`, `ico`
- [Render Textures](../../../examples/render-textures.atidarla/) — `square`, `dodeca`
- [Render Wire and Toon](../../../examples/render-wire-toon.atidarla/) — `ico`, `dodeca`
