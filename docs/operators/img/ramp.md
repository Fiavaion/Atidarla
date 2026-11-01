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

- [Beat switcher](../../../examples/beat-switcher.lasair/) — `gradient`
- [ComfyUI Invert](../../../examples/comfy-invert.lasair/) — `ramp`
- [Image processing](../../../examples/image-processing.lasair/) — `palette`
- [Keying](../../../examples/keying.lasair/) — `backdrop`
- [Pack: Beat Flash](../../../examples/pack-beat-flash.lasair/) — `wheel`
- [Pack: Film Grain](../../../examples/pack-film-grain.lasair/) — `sky`
- [Pack: LFO Bank](../../../examples/pack-lfo-bank.lasair/) — `backdrop`
- [PNT Capture](../../../examples/pnt-capture.lasair/) — `depth_map`
- [Render Textures](../../../examples/render-textures.lasair/) — `sphere_look`
- [Soak show](../../../examples/soak-show.lasair/) — `sunset`, `glow_ramp`
- [Stage deck](../../../examples/stage-deck.lasair/) — `sunset`, `glow_ramp`
- [Time effects](../../../examples/time-effects.lasair/) — `columns`
- [Two outputs](../../../examples/two-outputs.lasair/) — `sky`
