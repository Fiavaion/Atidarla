# Mesh File In (`msh.file_in`)

MSH · version 1 · cook budget 100 µs

Reads a static mesh from an OBJ, PLY or glTF 2.0 (.gltf, .glb) file: positions, polygons and lines, normals, texture coordinates and colours (glTF: the default scene with its node transforms).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `file` | File | path | `""` |  | An .obj, .ply, .gltf or .glb file (relative paths are relative to the project folder). |
| `obj_points` | OBJ Points | menu | `"split"` | `split`, `position` | OBJ only: split makes one point per distinct position, texture coordinate and normal (seams and hard edges stay exact); position makes one point per position (welded, attributes from its first use). |
| `normals` | Add Normals | toggle | `true` |  | Computes smooth normals `N` when the file has none. |

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.atidarla/) — `vase`
