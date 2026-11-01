# Line (`mat.line`)

MAT · version 1 · cook budget 20 µs

Wireframe: the edges of every polygon (not the diagonals the renderer adds to draw it) and every polyline, antialiased, a set number of pixels wide; inside the polygons a fill colour, or nothing so the back edges show.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Edge colour. |
| `width` | Width | float | `1.5` | 0.25 – 32 | Edge width in pixels. |
| `fill` | Fill | toggle | `false` |  | Fill the polygons with Fill Colour (hidden-line look); off: only edges are drawn. |
| `fill_colour` | Fill Colour | colour | `[0.0,0.0,0.0,1.0]` |  | Colour inside the polygons when Fill is on. |
| `use_cd` | Use Cd | toggle | `false` |  | Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one. |

## Platforms

All platforms.

## Examples

- [Render Wire and Toon](../../../examples/render-wire-toon.lasair/) — `ink`
