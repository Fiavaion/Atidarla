# Composite (`img.composite`)

IMG · version 1 · cook budget 120 µs

Combines input A with input B using a blend operation.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `a` | Img | optional |
| `b` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `operation` | Operation | menu | `"over"` | `over`, `add`, `multiply`, `screen`, `subtract`, `difference`, `maximum`, `minimum`, `average` | How A combines with B. `over` places A on top of B using A's alpha. |
| `opacity` | Opacity | float | `1.0` | 0 – 1 | Mix between B alone (0) and the full result (1). |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.lasair/) — `composite1`
- [Compute Shaders](../../../examples/compute-shaders.lasair/) — `glow`, `screen`
- [Image processing](../../../examples/image-processing.lasair/) — `outlined`, `masked`
- [Keying](../../../examples/keying.lasair/) — `over`
- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.lasair/) — `mix`, `out`
- [Neural Growth](../../../examples/neural-growth.lasair/) — `screen`
- [Pack: Colour Grade](../../../examples/pack-colour-grade.lasair/) — `masked`, `split`
- [Pack: Film Grain](../../../examples/pack-film-grain.lasair/) — `scene`
- [Pack: Kaleidoscope Tunnel](../../../examples/pack-kaleido-tunnel.lasair/) — `together`
- [Pack: LFO Bank](../../../examples/pack-lfo-bank.lasair/) — `stack`
- [Pack: RGB Delay](../../../examples/pack-rgb-delay.lasair/) — `shapes`
- [PNT Capture](../../../examples/pnt-capture.lasair/) — `stage`
- [PNT Constellation](../../../examples/pnt-constellation.lasair/) — `sky`
- [PNT Fields](../../../examples/pnt-fields.lasair/) — `stage`
- [PNT Motion](../../../examples/pnt-motion.lasair/) — `blob_and_cage`, `stage`, `final`
- [Points](../../../examples/points.lasair/) — `mix`
- [Signal scope](../../../examples/signal-scope.lasair/) — `scopes`, `all`
- [Simulations](../../../examples/simulations.lasair/) — `screen`, `all`
- [Soak show](../../../examples/soak-show.lasair/) — `over`
- [Time effects](../../../examples/time-effects.lasair/) — `compare`
- [Timeline show](../../../examples/timeline-show.lasair/) — `mix`
- [Two outputs](../../../examples/two-outputs.lasair/) — `sky_clouds`, `scene`
