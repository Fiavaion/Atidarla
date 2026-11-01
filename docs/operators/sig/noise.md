# Noise (`sig.noise`)

SIG · version 1 · cook budget 20 µs

Smooth random channels that wander over time.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `channels` | Channels | int | `1` | 1 – 8 | Number of independent channels (chan1…chan8). |
| `frequency` | Frequency | float | `1.0` |  | Changes per second. |
| `amplitude` | Amplitude | float | `1.0` |  | Output = offset + amplitude × noise (noise is in −1…1). |
| `offset` | Offset | float | `0.0` |  | Added to the scaled noise. |
| `seed` | Seed | int | `0` |  | Selects a different random sequence. |

## Platforms

All platforms.

## Examples

- [Simulations](../../../examples/simulations.lasair/) — `wobble`
