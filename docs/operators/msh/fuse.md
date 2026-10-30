# Fuse (`msh.fuse`)

MSH · version 1 · cook budget 100 µs

Welds points closer than a distance into one and removes primitives that collapse.

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
| `distance` | Distance | float | `0.001` | ≥ 0 | Points closer than this become one (the lowest-numbered keeps its attributes). |

## Platforms

All platforms.

## Examples

- [Mesh Boolean to glTF](../../../examples/mesh-boolean-gltf.atidarla/) — `weld`
- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `weld`
