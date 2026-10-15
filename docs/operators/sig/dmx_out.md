# DMX Out (`sig.dmx_out`)

SIG · version 1 · cook budget 20 µs

Sends input channels (0–1) as DMX512 slots from Start, over Art-Net or sACN (E1.31). The universe is sent when it changes (at most 44 times a second) and repeated every second.

Sink: cooked every frame even when nothing downstream is viewed, because it has external side effects.

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
| `protocol` | Protocol | menu | `"artnet"` | `artnet`, `sacn` | `artnet` (UDP 6454) or `sacn` (E1.31, UDP 5568). |
| `host` | Host | string | `""` |  | Destination node; empty broadcasts (Art-Net) or uses the universe's multicast group (sACN). |
| `port` | Port | int | `0` | 0 – 65535 | UDP port; 0 for the protocol's standard port. |
| `universe` | Universe | int | `1` | 0 – 63999 | Art-Net port address (0–32767) or sACN universe (1–63999). |
| `start` | Start | int | `1` | 1 – 512 | DMX slot (channel) for the first input channel. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
