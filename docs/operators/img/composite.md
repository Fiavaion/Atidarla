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

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `composite1`
- [Compute Shaders](../../../examples/compute-shaders.atidarla/) — `glow`, `screen`
- [Image processing](../../../examples/image-processing.atidarla/) — `outlined`, `masked`
- [Keying](../../../examples/keying.atidarla/) — `over`
- [Scatter to Iso Surface](../../../examples/mesh-scatter-iso.atidarla/) — `mix`, `out`
- [Neural Growth](../../../examples/neural-growth.atidarla/) — `screen`
- [Pack: Colour Grade](../../../examples/pack-colour-grade.atidarla/) — `masked`, `split`
- [Pack: Film Grain](../../../examples/pack-film-grain.atidarla/) — `scene`
- [Pack: Kaleidoscope Tunnel](../../../examples/pack-kaleido-tunnel.atidarla/) — `together`
- [Pack: LFO Bank](../../../examples/pack-lfo-bank.atidarla/) — `stack`
- [Pack: RGB Delay](../../../examples/pack-rgb-delay.atidarla/) — `shapes`
- [PNT Capture](../../../examples/pnt-capture.atidarla/) — `stage`
- [PNT Constellation](../../../examples/pnt-constellation.atidarla/) — `sky`
- [PNT Fields](../../../examples/pnt-fields.atidarla/) — `stage`
- [PNT Motion](../../../examples/pnt-motion.atidarla/) — `blob_and_cage`, `stage`, `final`
- [Points](../../../examples/points.atidarla/) — `mix`
- [Signal scope](../../../examples/signal-scope.atidarla/) — `scopes`, `all`
- [Simulations](../../../examples/simulations.atidarla/) — `screen`, `all`
- [Soak show](../../../examples/soak-show.atidarla/) — `over`
- [Time effects](../../../examples/time-effects.atidarla/) — `compare`
- [Timeline show](../../../examples/timeline-show.atidarla/) — `mix`
- [Two outputs](../../../examples/two-outputs.atidarla/) — `sky_clouds`, `scene`
