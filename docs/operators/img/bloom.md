# Bloom (`img.bloom`)

IMG · version 1 · cook budget 800 µs

Adds a glow around bright areas.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `threshold` | Threshold | float | `0.7` |  | Luminance above which pixels glow. |
| `size` | Size | float | `12.0` | 0 – 256 | Glow radius in pixels. |
| `intensity` | Intensity | float | `1.0` | ≥ 0 | Strength of the added glow. |

## Platforms

All platforms.

## Examples

- [Kaleidoscope tunnel](../../../examples/kaleidoscope-tunnel.lasair/) — `glow`
- [Particles](../../../examples/particles.lasair/) — `glow`
- [PNT Constellation](../../../examples/pnt-constellation.lasair/) — `glow`
- [PNT For-Each](../../../examples/pnt-foreach.lasair/) — `glow`
- [PNT Fountain](../../../examples/pnt-fountain.lasair/) — `glow`
- [PNT Solver](../../../examples/pnt-solver.lasair/) — `glow`
- [Render Solids](../../../examples/render-solids.lasair/) — `bloom`
- [Soak show](../../../examples/soak-show.lasair/) — `bloom`
