# Select (`sig.select`)

SIG · version 1 · cook budget 20 µs

Keeps only the named channels.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `channels` | Channels | string | `"chan1"` |  | Space-separated channel names, in output order. |

## Platforms

All platforms.

## Examples

- [Beat clock](../../../examples/beat-clock.atidarla/) — `cycle_fraction`
- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `bar_phase`, `beat_phase`
- [Live control](../../../examples/live-control.atidarla/) — `x`, `y`
- [Signal scope](../../../examples/signal-scope.atidarla/) — `luma`
