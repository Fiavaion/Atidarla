# Render Points (`img.render_points`)

IMG · version 1 · cook budget 150 µs

Draws the points of a geometry as square sprites over a background, looking down the Z axis (orthographic), and optionally its polyline primitives as one-pixel lines (such as Connect's). Colour from the `Cd` attribute or a constant; additive blending makes dense regions glow.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `geo` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `draw` | Draw | menu | `"points"` | `points`, `lines`, `both` | points: sprites; lines: the polyline primitives' segments, one pixel wide, coloured from their points (polygons are left to the mesh renderer); both: lines under sprites. |
| `size` | Size | float | `2.0` | 0.5 – 64 | Sprite size in pixels. |
| `colour_from` | Colour From | menu | `"attribute"` | `attribute`, `constant` | attribute: each point's `Cd` times Colour (Colour alone when `Cd` is missing); constant: Colour. |
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Sprite colour, or the multiplier of `Cd`. |
| `intensity` | Intensity | float | `1.0` | ≥ 0 | Multiplies the sprite colour (useful with additive blending). |
| `blend` | Blend | menu | `"add"` | `add`, `over` | add: sprites add up (glow, order-independent); over: alpha blending in point order. |
| `centre` | Centre | vec2 | `[0.0,0.0]` |  | The point of the XY plane at the image centre. |
| `height` | Height | float | `2.0` | ≥ 0.000001 | World units from the bottom to the top of the image (X follows the aspect ratio). |
| `background` | Background | colour | `[0.0,0.0,0.0,1.0]` |  | Colour the image is cleared to. |

## Platforms

All platforms.

## Examples

- [Kernel Particles](../../../examples/kernel-particles.lasair/) — `draw`
- [Mesh Boolean to glTF](../../../examples/mesh-boolean-gltf.lasair/) — `draw`
- [Mesh Files](../../../examples/mesh-files.lasair/) — `draw`
- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `draw`
- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `draw`
- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.lasair/) — `draw_blob`, `draw_seeds`, `draw_frame`
- [Particles](../../../examples/particles.lasair/) — `draw`
- [PNT Capture](../../../examples/pnt-capture.lasair/) — `draw`, `draw_body`
- [PNT Constellation](../../../examples/pnt-constellation.lasair/) — `draw`, `draw_stars`
- [PNT Deformers](../../../examples/pnt-deformers.lasair/) — `draw`
- [PNT Fields](../../../examples/pnt-fields.lasair/) — `draw`, `draw_swarm`
- [PNT For-Each](../../../examples/pnt-foreach.lasair/) — `draw`
- [PNT Fountain](../../../examples/pnt-fountain.lasair/) — `draw`
- [PNT Motion](../../../examples/pnt-motion.lasair/) — `draw_blob`, `draw_cage`, `draw_flock`, `draw_wrap`
- [PNT Outputs](../../../examples/pnt-outputs.lasair/) — `draw`
- [PNT Solver](../../../examples/pnt-solver.lasair/) — `draw`
- [Points](../../../examples/points.lasair/) — `draw_cloud`, `draw_grid`
- [Soak show](../../../examples/soak-show.lasair/) — `draw`
