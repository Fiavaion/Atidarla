# Delete (`msh.delete`)

MSH · version 1 · cook budget 100 µs

Deletes the primitives or points of a group (or everything outside it). Deleting points deletes the primitives that use them.

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
| `group` | Group | string | `""` |  | The group whose elements are deleted (empty: none, or all with Invert). |
| `class` | Class | menu | `"prim"` | `prim`, `point` | prim: deletes primitives; point: deletes points and the primitives using them. |
| `invert` | Invert | toggle | `false` |  | Deletes everything outside the group instead. |
| `keep_points` | Keep Points | toggle | `false` |  | prim: keeps points no remaining primitive uses. |

## Platforms

All platforms.

## Examples

- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `halve`
