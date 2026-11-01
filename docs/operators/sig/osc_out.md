# OSC Out (`sig.osc_out`)

SIG · version 1 · cook budget 20 µs

Sends each input channel as an OSC float message `<prefix>/<channel>` whenever the input changes, and passes it through.

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

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
| `host` | Host | string | `"127.0.0.1"` |  | Destination host name or IP address. |
| `port` | Port | int | `9001` | 1 – 65535 | Destination UDP port. |
| `prefix` | Prefix | string | `"/lsr"` |  | Address prefix for every message. |

## Platforms

All platforms.

## Examples

- [Live control](../../../examples/live-control.lasair/) — `osc_out1`
