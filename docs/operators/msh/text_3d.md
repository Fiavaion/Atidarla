# Text 3D (`msh.text_3d`)

MSH · version 1 · cook budget 100 µs

Extruded letters from a built-in or TrueType/OpenType font: filled glyph outlines (holes kept) with sides, in the groups `front`, `back` and `side`.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `text` | Text | string | `"Atidarla"` |  | The text; several lines are allowed. |
| `font` | Font | menu | `"sans"` | `sans`, `mono` | Built-in font: `sans` (Ubuntu Light) or `mono` (Hack). |
| `font_file` | Font File | path | `""` |  | Path to a .ttf or .otf file; overrides Font when set. |
| `size` | Size | float | `0.5` | ≥ 0 | The font size (its em height) in world units. |
| `depth` | Depth | float | `0.1` | ≥ 0 | Thickness along Z, centred on Z = 0; 0 makes flat letters (front faces only). |
| `spacing` | Line Spacing | float | `1.0` | ≥ 0 | Distance between lines, as a multiple of the font's line height. |
| `align` | Align | menu | `"centre"` | `left`, `centre`, `right` | Horizontal alignment of the lines and of the block on the origin. |
| `valign` | Vertical Align | menu | `"middle"` | `top`, `middle`, `baseline`, `bottom` | Which part of the block sits on the origin (baseline: the first line's). |
| `tolerance` | Curve Tolerance | float | `0.002` | ≥ 0 | Largest gap between a curve and its straight segments, as a fraction of the size (smaller: more points). |

## Platforms

All platforms.

## Examples

- [Mesh Construction](../../../examples/mesh-construction.atidarla/) — `title`
