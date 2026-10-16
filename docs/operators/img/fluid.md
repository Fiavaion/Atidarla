# Fluid 2D (`img.fluid`)

IMG · version 1 · cook budget 2500 µs

An incompressible 2D fluid carrying coloured dye. A built-in emitter and the `dye` and `velocity` images stir it; Reset clears it.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `dye` | Img | optional |
| `velocity` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `detail` | Detail | float | `0.5` | 0.1 – 1 | Simulation grid size as a fraction of the output (lower is faster and smoother). |
| `iterations` | Pressure Iterations | int | `20` | 1 – 80 | Pressure solver steps per frame; more keeps the fluid closer to incompressible. |
| `velocity_dissipation` | Velocity Fade | float | `0.2` | ≥ 0 | How fast motion dies away, per second. |
| `dye_dissipation` | Dye Fade | float | `0.3` | ≥ 0 | How fast dye fades, per second. |
| `emitter` | Emitter | toggle | `true` |  | A round source of dye and motion. |
| `emitter_position` | Emitter Position | vec2 | `[0.5,0.2]` |  | Centre as a fraction of the image from the bottom-left. |
| `emitter_radius` | Emitter Radius | float | `0.05` | ≥ 0 | Radius as a fraction of the image height. |
| `emitter_velocity` | Emitter Velocity | vec2 | `[0.0,0.5]` |  | Push in image heights per second; +y is up. |
| `emitter_colour` | Emitter Colour | colour | `[1.0,0.4,0.1,1.0]` |  | Dye the emitter adds. |
| `input_strength` | Input Strength | float | `1.0` | ≥ 0 | Scale for dye from `dye` and motion from `velocity` (rg mapped to −1…1, +y up). |
| `reset` | Reset | toggle | `false` |  | While on, clears the fluid. |

## Platforms

All platforms.

## Examples

- [Simulations](../../../examples/simulations.atidarla/) — `smoke`
