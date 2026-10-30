# Wave (`pnt.wave`)

PNT · version 1 · cook budget 60 µs

Displaces `P` by a sine wave: along Direction, with the wave travelling along Travel (linear) or outward from Origin in the plane across Travel (radial, ripples). Animate Phase (degrees) to make it move.

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
| `mode` | Mode | menu | `"linear"` | `linear`, `radial` | linear: crests are planes across Travel; radial: rings around the Travel axis through Origin. |
| `origin` | Origin | vec3 | `[0.0,0.0,0.0]` |  | Where the phase is measured from. |
| `travel` | Travel | vec3 | `[1.0,0.0,0.0]` |  | linear: direction the wave travels; radial: the axis of the rings. |
| `direction` | Direction | vec3 | `[0.0,1.0,0.0]` |  | Direction points are displaced in. |
| `amplitude` | Amplitude | float | `0.1` |  | Largest displacement. |
| `wavelength` | Wavelength | float | `1.0` | ≥ 0.000001 | Distance from crest to crest. |
| `phase` | Phase | float | `0.0` |  | Shift of the wave in degrees (for example the expression `time * 180`). |
| `decay` | Decay | float | `0.0` | ≥ 0 | Amplitude lost per unit of distance from the origin (exponential; 0 keeps it). |

## Platforms

All platforms.

## Examples

- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `ripple`
- [PNT For-Each](../../../examples/pnt-foreach.atidarla/) — `ripple`
