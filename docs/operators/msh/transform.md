# Transform (`msh.transform`)

MSH · version 1 · cook budget 100 µs

Scales, rotates and moves the geometry about a pivot. Normals follow; a mirror flips polygons so they still face outward.

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
| `translate` | Translate | vec3 | `[0.0,0.0,0.0]` |  | Moves the geometry. |
| `rotate` | Rotate | vec3 | `[0.0,0.0,0.0]` |  | Degrees about X, then Y, then Z. |
| `scale` | Scale | vec3 | `[1.0,1.0,1.0]` |  | Scale along X, Y and Z. |
| `uniform` | Uniform Scale | float | `1.0` |  | Multiplies the scale on every axis. |
| `pivot` | Pivot | vec3 | `[0.0,0.0,0.0]` |  | The point that scaling and rotation keep in place. |

## Platforms

All platforms.

## Examples

- [Mesh Boolean to glTF](../../../examples/mesh-boolean-gltf.lasair/) — `tilt`
- [Mesh Construction](../../../examples/mesh-construction.lasair/) — `raise`, `left`, `middle`, `right`
- [Mesh Files](../../../examples/mesh-files.lasair/) — `place_vase`, `place_loft`
- [Mesh Modelling](../../../examples/mesh-modelling.lasair/) — `tilt`
- [Mesh Primitives](../../../examples/mesh-primitives.lasair/) — `pulse`
- [VAT Playback](../../../examples/mesh-vat.lasair/) — `lower`, `raise`
