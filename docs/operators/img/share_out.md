# Share Out (`img.share_out`)

IMG · version 1 · cook budget 150 µs

Publishes its input to other applications on this computer (a Spout sender on Windows) and passes it through. Receivers get 8-bit BGRA at the input's size. If the name is taken, the sender becomes `name_1`, `name_2`, ...; the published name shows in the parameter panel. A Share In in the same project can receive it (a loopback): give the Share Out a name of its own and type it into the Share In. Spout needs the DX12 graphics backend (elsewhere the node reports an error); Syphon on macOS is not wired yet.

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
| `name` | Name | string | `"Atidarla"` |  | Sender name other applications see: 1-255 bytes, no backslash. |
| `enabled` | Enabled | toggle | `true` |  | Publish the input; off removes the sender. |

## Platforms

- Requires `win`; elsewhere: Texture sharing uses Spout and runs on Windows only for now: Syphon on macOS is not wired yet, and Linux has no sharing backend. The node loads and outputs nothing.

## Examples

- [Texture sharing](../../../examples/texture-sharing.atidarla/) — `share_out1`
