# Ramp (`img.ramp`)

IMG · version 1 · cook budget 80 µs

A gradient between two colours: horizontal, vertical, radial or angular.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `type` | Type | menu | `"horizontal"` | `horizontal`, `vertical`, `radial`, `angular` | Direction of the gradient; radial runs from the centre outwards. |
| `colour_a` | Colour A | colour | `[0.0,0.0,0.0,1.0]` |  | Colour at the start. |
| `colour_b` | Colour B | colour | `[1.0,1.0,1.0,1.0]` |  | Colour at the end. |
| `phase` | Phase | float | `0.0` |  | Shifts the gradient position; the result is clamped to the two colours. |

## Platforms

All platforms.

## Examples

- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `gradient`
- [ComfyUI Invert](../../../examples/comfy-invert.atidarla/) — `ramp`
- [Image processing](../../../examples/image-processing.atidarla/) — `palette`
- [Keying](../../../examples/keying.atidarla/) — `backdrop`
- [Pack: Beat Flash](../../../examples/pack-beat-flash.atidarla/) — `wheel`
- [Pack: Film Grain](../../../examples/pack-film-grain.atidarla/) — `sky`
- [Pack: LFO Bank](../../../examples/pack-lfo-bank.atidarla/) — `backdrop`
- [Render Textures](../../../examples/render-textures.atidarla/) — `sphere_look`
- [Stage deck](../../../examples/stage-deck.atidarla/) — `sunset`, `glow_ramp`
- [Time effects](../../../examples/time-effects.atidarla/) — `columns`
- [Two outputs](../../../examples/two-outputs.atidarla/) — `sky`
