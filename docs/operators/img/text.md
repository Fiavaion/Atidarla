# Text (`img.text`)

IMG · version 1 · cook budget 80 µs

Draws text (several lines allowed) over the input or a background colour, in a built-in or TrueType/OpenType font.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `text` | Text | string | `"Atidarla"` |  | The text; several lines are allowed. |
| `font` | Font | menu | `"sans"` | `sans`, `mono` | Built-in font: `sans` (Ubuntu Light) or `mono` (Hack). |
| `font_file` | Font File | path | `""` |  | Path to a .ttf or .otf file; overrides Font when set. |
| `size` | Size | float | `0.1` | ≥ 0 | Line height as a fraction of the image height. |
| `spacing` | Line Spacing | float | `1.0` | ≥ 0 | Distance between lines, as a multiple of the font's line height. |
| `position` | Position | vec2 | `[0.5,0.5]` |  | Anchor point as a fraction of the image from the bottom-left. |
| `align` | Align | menu | `"centre"` | `left`, `centre`, `right` | Horizontal alignment of the lines and of the block on the anchor. |
| `valign` | Vertical Align | menu | `"middle"` | `top`, `middle`, `bottom` | Which part of the block sits on the anchor. |
| `colour` | Colour | colour | `[1.0,1.0,1.0,1.0]` |  | Text colour. |
| `background` | Background | colour | `[0.0,0.0,0.0,0.0]` |  | Colour behind the text when nothing is connected to `in`. |

## Platforms

All platforms.

## Examples

- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `title`
- [Pack: Chromatic Aberration](../../../examples/pack-chromatic-aberration.atidarla/) — `title`
- [Pack: Duotone](../../../examples/pack-duotone.atidarla/) — `title`
- [Pack: Film Grain](../../../examples/pack-film-grain.atidarla/) — `title`
- [Pack: Neon Edges](../../../examples/pack-neon-edges.atidarla/) — `label`
- [Stage deck](../../../examples/stage-deck.atidarla/) — `title`
