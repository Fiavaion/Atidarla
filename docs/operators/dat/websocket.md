# WebSocket (`dat.websocket`)

DAT · version 1 · cook budget 40 µs

A WebSocket client on a worker thread: messages it receives become rows of its output table (a `message` header row, then the newest `Keep` messages, newest last), and each new value on its `send` input is sent as one text message. Reconnects every second while Active and the connection is lost. `ws://` or `wss://` (TLS, the certificate checked against the operating system's trust store); either way the connection goes through Sovereign mode's egress gate: this machine and the LAN are always reachable, the internet only with Sovereign mode off.

Cooks every frame (time-dependent).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `send` | Dat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `url` | URL | string | `""` |  | `ws://host:port/path` or `wss://host:port/path` of the server, for example `ws://127.0.0.1:8080/`. |
| `active` | Active | toggle | `true` |  | Connects while on; off closes the connection. |
| `keep` | Keep | int | `20` | 1 – 1000 | Messages kept in the output table. |

## Platforms

All platforms.

## Examples

- [DAT network](../../../examples/dat-network.lasair/) — `socket`
