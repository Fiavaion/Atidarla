# Clip (`msh.clip`)

MSH · version 1 · cook budget 100 µs

Cuts the geometry with a plane and keeps one side or both (groups `above` and `below`), welded along the cut. Cap fills the cut (group `clip_cap`) so closed input stays closed.

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
| `origin` | Origin | vec3 | `[0.0,0.0,0.0]` |  | A point on the plane. |
| `normal` | Normal | vec3 | `[0.0,1.0,0.0]` |  | The plane's normal; it points to the side called above. |
| `keep` | Keep | menu | `"above"` | `above`, `below`, `both` | above: keeps what is on the normal's side; below: the other side; both: keeps both halves as separate pieces, the groups `above` and `below`. |
| `cap` | Cap | toggle | `false` |  | Fills the cut with polygons (the group `clip_cap`) so a closed input stays closed. |

## Platforms

All platforms.

## Examples

- [Mesh Shaping](../../../examples/mesh-shaping.atidarla/) — `cut`, `half`
