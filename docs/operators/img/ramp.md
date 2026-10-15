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

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
