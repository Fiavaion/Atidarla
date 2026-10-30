# Share Out (`img.share_out`)

IMG · version 1 · cook budget 150 µs

Publishes its input to other applications on this computer (a Spout sender on Windows, a Syphon server on macOS) and passes it through. Receivers get 8-bit BGRA at the input's size. Spout: if the name is taken, the sender becomes `name_1`, `name_2`, ...; Syphon allows shared names. The published name shows in the parameter panel. A Share In in the same project can receive it (a loopback): give the Share Out a name of its own and type it into the Share In. Spout needs the DX12 graphics backend and Syphon the Metal one (the defaults; elsewhere the node reports an error).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `name` | Name | string | `"Atidarla"` |  | Sender name other applications see: up to 255 bytes; on Windows at least 1 byte and no backslash. |
| `enabled` | Enabled | toggle | `true` |  | Publish the input; off removes the sender. |

## Platforms

- Requires `win|mac`; elsewhere: Texture sharing uses Spout on Windows and Syphon on macOS; Linux has no sharing backend. The node loads and outputs nothing.

## Examples

- [Texture sharing](../../../examples/texture-sharing.atidarla/) — `share_out1`
