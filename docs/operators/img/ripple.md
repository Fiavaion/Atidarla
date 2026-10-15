# Ripple (`img.ripple`)

IMG · version 1 · cook budget 500 µs

A water-surface simulation: bright areas of the `drive` input push the surface, and waves spread and fade.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `drive` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `strength` | Strength | float | `0.3` |  | How strongly the drive input pushes the surface each step. |
| `damping` | Damping | float | `0.985` | 0 – 1 | Fraction of wave height kept per step. |
| `steps` | Steps | int | `2` | 1 – 16 | Simulation steps per frame (wave speed). |
| `gain` | Gain | float | `2.0` |  | Output brightness = 0.5 + height × gain. |
| `reset` | Reset | toggle | `false` |  | While on, flattens the surface. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
