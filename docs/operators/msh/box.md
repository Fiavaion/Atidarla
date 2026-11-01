# Box (`msh.box`)

MSH · version 1 · cook budget 100 µs

A closed box of quads, welded at its edges, with smooth point normals `N` (add Normals in flat mode for hard edges).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `size` | Size | vec3 | `[1.0,1.0,1.0]` |  | Extent along X, Y and Z. |
| `divisions` | Divisions | int | `1` | 1 – 128 | Quads along each edge. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |

## Platforms

All platforms.

## Examples

- [Mesh Boolean to glTF](../../../examples/mesh-boolean-gltf.lasair/) — `cutter`
- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `block`, `cutter`
- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `box`
- [Mesh Shaping](../../../examples/mesh-shaping.lasair/) — `block`
