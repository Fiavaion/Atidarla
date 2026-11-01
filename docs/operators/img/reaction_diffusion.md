# Reaction-Diffusion (`img.reaction_diffusion`)

IMG · version 1 · cook budget 1500 µs

A Gray-Scott reaction-diffusion simulation growing organic patterns from a seed; bright areas of the `seed` input add more.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `seed` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `feed` | Feed | float | `0.055` | 0 – 0.1 | Feed rate of chemical A. |
| `kill` | Kill | float | `0.062` | 0 – 0.1 | Removal rate of chemical B; feed/kill pairs select the pattern family. |
| `steps` | Steps | int | `8` | 1 – 64 | Simulation steps per frame (growth speed). |
| `colour_a` | Colour A | colour | `[0.0,0.0,0.0,1.0]` |  | Colour where B is absent. |
| `colour_b` | Colour B | colour | `[1.0,1.0,1.0,1.0]` |  | Colour where B is present. |
| `reset` | Reset | toggle | `false` |  | While on, restarts from the centre seed. |

## Platforms

All platforms.

## Examples

- [Simulations](../../../examples/simulations.lasair/) — `pattern`
