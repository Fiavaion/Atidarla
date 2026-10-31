# Scatter (`pnt.scatter`)

PNT · version 1 · cook budget 150 µs

Random points, uniformly distributed on a shape: a line segment, a circle or a disk, a rectangle, a sphere's surface, a ball or a box. Writes `P`, `id` and, on the flat and spherical shapes, the surface normal `N`. Seeded: the same seed gives the same points. (Scattering on a mesh surface comes with the MSH operators.)

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `shape` | Shape | menu | `"disk"` | `line`, `circle`, `disk`, `rectangle`, `sphere`, `ball`, `box` | line: Start to End; circle, disk: radius Radius in the XY plane; rectangle: Size X by Y in the XY plane; sphere: the surface of radius Radius; ball: inside it; box: inside Size. Then rotated by Rotate and moved to Centre. |
| `count` | Count | int | `10000` | 0 – 16000000 | Number of points. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | Centre of the shape (not used by line). |
| `radius` | Radius | float | `1.0` | ≥ 0 | circle, disk, sphere, ball: the radius. |
| `inner` | Inner | float | `0.0` | 0 – 1 | disk, ball: the hole's radius, as a fraction of Radius. |
| `size` | Size | vec3 | `[2.0,2.0,2.0]` |  | rectangle, box: the extent along X, Y and Z. |
| `start` | Start | vec3 | `[-1.0,0.0,0.0]` |  | line: one end. |
| `end` | End | vec3 | `[1.0,0.0,0.0]` |  | line: the other end. |
| `rotate` | Rotate | vec3 | `[0.0,0.0,0.0]` |  | Rotation of the shape about X, Y, Z in degrees (not used by line). |
| `seed` | Seed | int | `1` | ≥ 0 | Random seed. |

## Platforms

All platforms.

## Examples

- [Kernel Particles](../../../examples/kernel-particles.atidarla/) — `seed`
- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `sphere`, `field`
- [PNT Deformers](../../../examples/pnt-deformers.atidarla/) — `column`, `ring`
- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `brushes`, `swarm`
- [PNT For-Each](../../../examples/pnt-foreach.atidarla/) — `column`, `strip`
- [PNT Fountain](../../../examples/pnt-fountain.atidarla/) — `ring`
- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `blob`, `flock_start`, `dust`
- [PNT Solver](../../../examples/pnt-solver.atidarla/) — `disk`
