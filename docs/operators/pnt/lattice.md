# Lattice (`pnt.lattice`)

PNT · version 1 · cook budget 80 µs

Free-form deformation: a box of control points (Divisions along X, Y and Z, filling Size around Centre) bends everything inside it as the control points move; each point follows the trilinear blend of the displacements around it (points outside the box follow its nearest face). The displaced control points come from the `cage` input (one point per control point, X fastest, then Y, then Z: for example this operator's cage output sent through Twist or Noise) or, unconnected, from smooth noise of Amplitude. Output cage shows the displaced control points, with the lattice's edges as lines.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `cage` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `divisions` | Divisions | vec3 | `[3.0,3.0,3.0]` |  | Control points along X, Y and Z (2 to 16 each). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | The centre of the lattice's box. |
| `size` | Size | vec3 | `[2.0,2.0,2.0]` |  | The box's extent along X, Y and Z. |
| `amplitude` | Amplitude | float | `0.3` |  | Without a cage input: how far noise moves the control points. |
| `frequency` | Frequency | float | `1.0` | ≥ 0 | Without a cage input: the noise's cells per unit. |
| `evolution` | Evolution | float | `0.0` |  | Without a cage input: moves through the noise; drive it from time to make the lattice breathe. |
| `seed` | Seed | int | `0` | ≥ 0 | Without a cage input: selects a different noise. |
| `output` | Output | menu | `"deformed"` | `deformed`, `cage` | deformed: the input, deformed; cage: the displaced control points, with the lattice's edges as lines. |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `squash`, `cage`
