# Render (`img.render`)

IMG · version 1 · cook budget 250 µs

Draws up to four geometries (`geo`, `geo2`…), each with the material wired beside it (`mat`, `mat2`…; a grey PBR surface coloured by `Cd` when none), through a perspective or orthographic camera, lit by one directional light and ambient light, with a depth buffer and antialiasing. Polygons are filled (normals from `N`, else faceted; texture coordinates from `uv`), polylines drawn as lines, and geometry without primitives as points.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `geo` | Geo | optional |
| `mat` | Mat | optional |
| `geo2` | Geo | optional |
| `mat2` | Mat | optional |
| `geo3` | Geo | optional |
| `mat3` | Mat | optional |
| `geo4` | Geo | optional |
| `mat4` | Mat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `projection` | Projection | menu | `"perspective"` | `perspective`, `orthographic` | perspective: Field of View; orthographic: Ortho Height, no foreshortening. |
| `camera` | Camera | menu | `"orbit"` | `orbit`, `look_at` | orbit: the camera circles Target at Distance, placed by Azimuth and Elevation (easy to animate); look_at: the camera sits at Position looking at Target. |
| `target` | Target | vec3 | `[0.0,0.0,0.0]` |  | The point the camera looks at. |
| `azimuth` | Azimuth | float | `30.0` |  | Orbit: degrees around the Y axis (0 looks from +Z, 90 from +X). |
| `elevation` | Elevation | float | `20.0` | -90 – 90 | Orbit: degrees above the XZ plane. |
| `distance` | Distance | float | `4.0` | ≥ 0 | Orbit: distance from Target. |
| `position` | Position | vec3 | `[0.0,0.0,4.0]` |  | Look at: the camera's position. |
| `fov` | Field of View | float | `45.0` | 1 – 179 | Perspective: vertical angle of view in degrees. |
| `ortho_height` | Ortho Height | float | `3.0` | ≥ 0.000001 | Orthographic: world units from the bottom to the top of the image. |
| `near` | Near | float | `0.05` | ≥ 0.000001 | Nearest distance drawn. |
| `far` | Far | float | `100.0` | ≥ 0.000001 | Farthest distance drawn. |
| `light_azimuth` | Light Azimuth | float | `40.0` |  | Direction the light comes from: degrees around the Y axis. |
| `light_elevation` | Light Elevation | float | `50.0` | -90 – 90 | Direction the light comes from: degrees above the XZ plane. |
| `light_colour` | Light Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Colour of the directional light. |
| `light_intensity` | Light Intensity | float | `1.0` | ≥ 0 | Multiplies Light Colour. |
| `ambient` | Ambient | colour | `[0.15,0.15,0.18,1.0]` |  | Light from every direction (added to lit materials). |
| `cull` | Cull | menu | `"back"` | `back`, `none`, `front` | Which polygon sides are not drawn: back (seen from behind; polygons wind counter-clockwise seen from their front), none, or front. |
| `antialias` | Antialias | toggle | `true` |  | 4× multisampling, with antialiased lines and sprite edges; off: aliased edges. |
| `wireframe` | Wireframe | toggle | `false` |  | Draw the polygon edges over every surface. |
| `wire_colour` | Wire Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Colour of the wireframe overlay (alpha: its opacity). |
| `wire_width` | Wire Width | float | `1.0` | 0.25 – 32 | Wireframe overlay width in pixels. |
| `point_size` | Point Size | float | `3.0` | 0.5 – 256 | Pixel size of the points of a geometry without primitives (Point Sprite materials set their own). |
| `background` | Background | colour | `[0.0,0.0,0.0,1.0]` |  | Colour the image is cleared to. |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `look`
- [Render Solids](../../../examples/render-solids.atidarla/) — `render`
- [Render Textures](../../../examples/render-textures.atidarla/) — `render`
- [Render Wire and Toon](../../../examples/render-wire-toon.atidarla/) — `render`
