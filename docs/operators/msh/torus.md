# Torus (`msh.torus`)

MSH · version 1 · cook budget 100 µs

A closed torus (ring) around Y, made of quads, with normals `N`.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `radius` | Radius | float | `0.5` | ≥ 0 | Distance from the centre to the middle of the tube. |
| `thickness` | Tube Radius | float | `0.2` | ≥ 0 | Radius of the tube. |
| `rows` | Rows | int | `12` | 3 – 4096 | Segments around the tube. |
| `columns` | Columns | int | `32` | 3 – 4096 | Segments around the ring. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |

## Platforms

All platforms.

## Examples

- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `donut`
- [Mesh Primitives](../../../examples/mesh-primitives.atidarla/) — `ring`
- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `ring`
