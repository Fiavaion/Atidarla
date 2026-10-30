# Share In (`img.share_in`)

IMG · version 1 · cook budget 150 µs

Receives an image another application publishes on this computer (a Spout sender on Windows, a Syphon server on macOS), or a Share Out's. The output takes the sender's size and follows it when it changes; while there is no sender it is black. Spout needs the DX12 graphics backend and Syphon the Metal one (the defaults; elsewhere the node reports an error).

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
| `name` | Sender | string | `""` |  | Sender to receive from; empty follows the active sender (Spout: the newest one, unless the user chose another in a Spout tool; Syphon: the newest server). A Syphon server matches by its name, by "App - Name" as Syphon apps list it, or by its app's name when it has no name of its own. |

## Platforms

- Requires `win|mac`; elsewhere: Texture sharing uses Spout on Windows and Syphon on macOS; Linux has no sharing backend. The node loads and outputs nothing.

## Examples

- [Texture sharing](../../../examples/texture-sharing.atidarla/) — `share_in1`
