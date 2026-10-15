# OSC In (`sig.osc_in`)

SIG · version 1 · cook budget 20 µs

Receives OSC on a UDP port; each numeric address becomes a channel (`/a/b` → `a_b`) holding its latest value.

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
| `port` | Port | int | `9000` | 0 – 65535 | UDP port to listen on (all interfaces). |
| `filter` | Filter | string | `""` |  | Only channels starting with this text. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
