# Grid (`msh.grid`)

MSH · version 1 · cook budget 100 µs

A flat rectangle of quads (a plane), with normals `N` and texture coordinates `uv`.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `plane` | Plane | menu | `"xy"` | `xy`, `xz`, `yz` | The plane the shape lies in (its normal is +Z for xy, +Y for xz, +X for yz). |
| `size` | Size | vec2 | `[2.0,2.0]` |  | Width and height. |
| `rows` | Rows | int | `10` | 2 – 4096 | Points along the second axis (v). |
| `columns` | Columns | int | `10` | 2 – 4096 | Points along the first axis (u). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |

## Platforms

All platforms.

## Examples

- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `ground`
