# Share In (`img.share_in`)

IMG · version 1 · cook budget 150 µs

Receives an image another application publishes on this computer (a Spout sender on Windows), or a Share Out's. The output takes the sender's size and follows it when it changes; while there is no sender it is black. Spout needs the DX12 graphics backend (elsewhere the node reports an error); Syphon on macOS is not wired yet.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name` | Sender | string | `""` |  | Sender to receive from; empty follows the active sender (the newest one, unless the user chose another in a Spout tool). |

## Platforms

- Requires `win`; elsewhere: Texture sharing uses Spout and runs on Windows only for now: Syphon on macOS is not wired yet, and Linux has no sharing backend. The node loads and outputs nothing.

## Examples

- [Texture sharing](../../../examples/texture-sharing.atidarla/) — `share_in1`
