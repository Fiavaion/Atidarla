# Math (`sig.math`)

SIG · version 1 · cook budget 20 µs

Combines A with B channel by channel, then maps the range from→to.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `a` | Sig | optional |
| `b` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `operation` | Operation | menu | `"add"` | `add`, `subtract`, `multiply`, `divide`, `maximum`, `minimum` | Applied when B is connected; B's first channel is used against all of A if B has fewer channels. |
| `from_min` | From Min | float | `0.0` |  | Input value that maps to To Min. |
| `from_max` | From Max | float | `1.0` |  | Input value that maps to To Max. |
| `to_min` | To Min | float | `0.0` |  | Output at From Min. |
| `to_max` | To Max | float | `1.0` |  | Output at From Max. |

## Platforms

All platforms.

## Examples

- [Audio-reactive feedback](../../../examples/audio-reactive-feedback.atidarla/) — `loudness`
- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `index`, `flash`, `spin`
- [Live control](../../../examples/live-control.atidarla/) — `degrees`
