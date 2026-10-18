# PBR (`mat.pbr`)

MAT · version 1 · cook budget 20 µs

A lit surface: Lambert diffuse plus GGX specular from the renderer's light, ambient light and emission. Albedo is multiplied by `Cd` and by the `map` image (at `uv`) when they are there; emission adds a glow that lighting does not darken.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `map` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Mat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `albedo` | Albedo | colour | `[0.8,0.8,0.8,1.0]` |  | Base colour of the surface. |
| `roughness` | Roughness | float | `0.5` | 0.02 – 1 | 0.02: a sharp mirror-like highlight; 1: a broad, dull one. |
| `metallic` | Metallic | float | `0.0` | 0 – 1 | 0: plastic-like (white highlights); 1: metal (highlights take the albedo, no diffuse). |
| `emission` | Emission | colour | `[0.0,0.0,0.0,1.0]` |  | Light the surface gives off, added after lighting. |
| `emission_strength` | Emission Strength | float | `1.0` | ≥ 0 | Multiplies Emission (above 1 for a glow that a later Bloom picks up). |
| `use_cd` | Use Cd | toggle | `true` |  | Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one. |

## Platforms

All platforms.

## Examples

- [Render Solids](../../../examples/render-solids.atidarla/) — `glossy`, `glow`
