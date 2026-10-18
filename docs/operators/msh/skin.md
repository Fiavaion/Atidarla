# Skin (`msh.skin`)

MSH · version 1 · cook budget 100 µs

Lofts a surface through the input's primitives in order (profiles with equal point counts), optionally as a closed ring and with end caps.

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
| `closed` | Closed | toggle | `false` |  | Also connects the last profile back to the first (a ring). |
| `caps` | Caps | toggle | `true` |  | Closes the first and last profiles with polygons (closed profiles, not a ring). |
| `reverse` | Reverse | toggle | `false` |  | Flips the surface (use it when it faces inward). |

## Platforms

All platforms.

## Examples

- [Mesh Files](../../../examples/mesh-files.atidarla/) — `loft`
