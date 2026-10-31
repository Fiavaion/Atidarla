# Web Server (`dat.web_server`)

DAT · version 1 · cook budget 40 µs

Serves its `content` input over HTTP on a port of this machine (`http://127.0.0.1:<port>/`, any path), and outputs the requests it received as a table (`method`, `path`, `body`; newest last), so a phone or another program can send text into the show with a POST. Listens on loopback only unless LAN is on. Connections are handled on a worker thread; requests over 1 MB are refused.

Cooks every frame (time-dependent).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `content` | Dat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `port` | Port | int | `9980` | 1 – 65535 | The TCP port to listen on. |
| `lan` | LAN | toggle | `false` |  | Also accepts connections from other machines (listens on every interface). Off: this machine only. |
| `content_type` | Content Type | menu | `"auto"` | `auto`, `text`, `json`, `csv`, `html` | How the content is sent: `auto` sends text as plain text and a table as TSV; `json` and `html` send text with that type; `csv` sends a table as CSV. |
| `keep` | Keep | int | `20` | 1 – 1000 | Requests kept in the output table. |

## Platforms

All platforms.

## Examples

- [DAT network](../../../examples/dat-network.atidarla/) — `server`
