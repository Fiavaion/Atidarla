# Colour by Attribute (`pnt.colour`)

PNT · version 1 · cook budget 60 µs

Colours points from an attribute: a component (or the length) of it, mapped from [Low, High] to 0-1, picks a colour from a ramp (two or three colours of your choice, or the heat, ice and rainbow presets) written to `Cd`.

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
| `attribute` | Attribute | string | `"P"` |  | The point attribute read (not `Cd`). |
| `measure` | Measure | menu | `"y"` | `x`, `y`, `z`, `w`, `length` | Which component, or the vector's length. |
| `low` | Low | float | `-1.0` |  | The value mapped to the start of the ramp. |
| `high` | High | float | `1.0` |  | The value mapped to its end. |
| `ramp` | Ramp | menu | `"two"` | `two`, `three`, `heat`, `ice`, `rainbow` | two: Colour 1 to Colour 2; three: Colour 1, Colour 2 in the middle, Colour 3; heat: black, red, yellow, white; ice: black, blue, cyan, white; rainbow: hues. |
| `colour1` | Colour 1 | colour | `[0.1,0.2,1.0,1.0]` |  | The start of the two- and three-colour ramps. |
| `colour2` | Colour 2 | colour | `[1.0,0.3,0.1,1.0]` |  | The end of the two-colour ramp, the middle of the three-colour one. |
| `colour3` | Colour 3 | colour | `[1.0,1.0,0.8,1.0]` |  | The end of the three-colour ramp. |
| `repeat` | Repeat | toggle | `false` |  | Repeats the ramp past the ends instead of holding the end colours. |

## Platforms

All platforms.

## Examples

- [Kernel Particles](../../../examples/kernel-particles.atidarla/) — `colour`
- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `shade`
- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `height`
- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `heat`, `by_crowd`
- [PNT Fountain](../../../examples/pnt-fountain.atidarla/) — `by_speed`
- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `blob_colour`, `birds`
