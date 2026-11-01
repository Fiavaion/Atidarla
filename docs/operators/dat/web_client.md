# Web Client (`dat.web_client`)

DAT · version 1 · cook budget 40 µs

Sends an HTTP or HTTPS request on a worker thread and outputs the response body as text (feed JSON to JSON). Requests go on the Fetch pulse, when the URL, method, headers or body change (On Change), and every Interval seconds. Every connection goes through Sovereign mode's egress gate: this machine and the LAN are always reachable, the internet only with Sovereign mode off. The status line shows the HTTP status, size and time; a status of 400 or more is a node error (the body is still output).

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `body` | Dat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `url` | URL | string | `""` |  | `http://` or `https://` address, for example `http://127.0.0.1:9980/` (a Web Server on this machine). |
| `method` | Method | menu | `"GET"` | `GET`, `POST`, `PUT`, `DELETE` | The HTTP method. POST and PUT send the body. |
| `headers` | Headers | string | `""` |  | Extra request headers, one `Name: value` per line. |
| `body` | Body | string | `""` |  | The request body when the `body` input is not connected. |
| `fetch` | Fetch | pulse | `false` |  | Sends the request now. |
| `on_change` | On Change | toggle | `true` |  | Sends whenever the URL, method, headers or body change (and once when the node starts). |
| `interval_s` | Interval | float | `0.0` | 0 – 3600 | Sends again every this many seconds; 0: never on its own. |
| `timeout_s` | Timeout | float | `10.0` | 0.5 – 600 | Seconds a request may take before it fails. |

## Platforms

All platforms.

## Examples

- [DAT network](../../../examples/dat-network.lasair/) — `client`
