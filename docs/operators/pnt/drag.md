# Drag (`pnt.drag`)

PNT · version 1 · cook budget 40 µs

Slows the velocity `v` down: linear drag (a fraction of the speed lost per second, frame-rate independent) or quadratic drag (stronger at high speed).

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
| `amount` | Amount | float | `1.0` | ≥ 0 | linear: decay rate per second (1: about 63% of the speed lost per second); quadratic: the coefficient of speed². |
| `mode` | Mode | menu | `"linear"` | `linear`, `quadratic` | linear: v × e^(−amount·dt); quadratic: v / (1 + amount·\|v\|·dt). |

## Platforms

All platforms.

## Examples

- [PNT Fountain](../../../examples/pnt-fountain.lasair/) — `air`
- [PNT Solver](../../../examples/pnt-solver.lasair/) — `damp`
