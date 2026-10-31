# Integrate (`pnt.integrate`)

PNT · version 1 · cook budget 40 µs

Moves points by their velocity for one frame: `P += v × dt × scale`. The step of a home-made simulation (Solver, or a feedback loop without Particles); Particles integrates by itself.

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
| `scale` | Scale | float | `1.0` |  | Multiplies the step (0 freezes, negative runs backwards). |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `fly`
- [PNT Solver](../../../examples/pnt-solver.atidarla/) — `move`
