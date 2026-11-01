# Pattern (`sig.pattern`)

SIG · version 1 · cook budget 20 µs

One channel of Length samples shaped by a waveform over Cycles; Speed scrolls it over time. Feed Channel Image to turn it into a texture, or index it from other operators.

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
| `shape` | Shape | menu | `"sine"` | `sine`, `triangle`, `square`, `ramp`, `noise`, `random` | Waveform; `noise` is smooth value noise, `random` an independent value per sample. |
| `length` | Length | int | `64` | 1 – 4096 | Number of samples. |
| `cycles` | Cycles | float | `1.0` |  | Waveform cycles across the whole pattern. |
| `phase` | Phase | float | `0.0` |  | Shift in cycles. |
| `speed` | Speed | float | `0.0` |  | Phase change per second (cycles); 0 holds the pattern still. |
| `amplitude` | Amplitude | float | `1.0` |  | Scale of the waveform (it spans −1…1 before scaling). |
| `offset` | Offset | float | `0.0` |  | Added after scaling. |
| `seed` | Seed | int | `0` |  | Seed for `noise` and `random`. |

## Platforms

All platforms.

## Examples

- [PNT Fields](../../../examples/pnt-fields.lasair/) — `wave`
- [Signal scope](../../../examples/signal-scope.lasair/) — `wave`
