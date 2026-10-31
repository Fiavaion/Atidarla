# Ray Intersect (`pnt.ray_intersect`)

PNT · version 1 · cook budget 500 µs

Casts a ray from every point of `in` (along Direction, along the point's normal `N`, or toward Centre) against the polygons of `target` and keeps the nearest hit within Max Distance: writes `hitdist` (the distance, or −1 for a miss) and, with Project on, moves the point onto the surface. Both Ways also casts backwards. Brute force on the GPU (points × triangles): for targets up to a few thousand triangles.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `target` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `direction_from` | Direction From | menu | `"vector"` | `vector`, `normal`, `point` | vector: Direction for every point; normal: each point's `N`; point: toward Centre. |
| `direction` | Direction | vec3 | `[0.0,-1.0,0.0]` |  | vector: the ray direction. |
| `centre` | Centre | vec3 | `[0.0,0.0,0.0]` |  | point: where the rays aim. |
| `max_distance` | Max Distance | float | `100.0` | ≥ 0 | Hits farther than this are ignored. |
| `both_ways` | Both Ways | toggle | `false` |  | Also casts in the opposite direction; the nearer hit wins. |
| `project` | Project | toggle | `true` |  | Moves each point that hits onto the surface. |

## Platforms

All platforms.

## Examples

- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `shrink_wrap`
