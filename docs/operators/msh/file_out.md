# Mesh File Out (`msh.file_out`)

MSH · version 1 · cook budget 100 µs

Writes its input to an OBJ, PLY or glTF 2.0 (.gltf, .glb) file whenever it changes, and passes it through. Files are byte-identical for the same geometry.

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
| `file` | File | path | `""` |  | Where to write (relative paths are relative to the project folder); empty writes nothing. |
| `format` | Format | menu | `"auto"` | `auto`, `obj`, `ply`, `gltf`, `glb` | auto: from the file's extension. |

## Platforms

All platforms.

## Examples

- [Mesh Boolean to glTF](../../../examples/mesh-boolean-gltf.lasair/) — `save`
- [Mesh Files](../../../examples/mesh-files.lasair/) — `save`
- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `export`
