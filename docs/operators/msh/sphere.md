# Sphere (`msh.sphere`)

MSH · version 1 · cook budget 100 µs

A closed sphere: polar rings of quads, or an evenly subdivided icosahedron. Writes `N`.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `type` | Type | menu | `"polar"` | `polar`, `ico` | polar: rings of quads between two poles (triangle fans); ico: a subdivided icosahedron of even triangles. |
| `radius` | Radius | float | `0.5` | ≥ 0 | Radius. |
| `rows` | Rows | int | `16` | 2 – 2048 | polar: bands from pole to pole. |
| `columns` | Columns | int | `32` | 3 – 4096 | polar: segments around. |
| `level` | Level | int | `3` | 0 – 7 | ico: subdivisions (20 × 4^level triangles). |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape. |

## Platforms

All platforms.

## Examples

- [Mesh Boolean to glTF](../../../examples/mesh-boolean-gltf.atidarla/) — `ball`
- [Mesh Construction](../../../examples/mesh-construction.atidarla/) — `ball`
- [Mesh Modelling](../../../examples/mesh-modelling.atidarla/) — `ball`
- [Mesh Primitives](../../../examples/mesh-primitives.atidarla/) — `ball`
- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.atidarla/) — `base`
- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `ball`
