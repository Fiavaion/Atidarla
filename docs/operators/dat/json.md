# JSON (`dat.json`)

DAT · version 1 · cook budget 300 µs

Parses JSON from its input (File In, Web Client, Text) or, with nothing connected, from the JSON parameter; `Path` picks a part of it (a JSON Pointer such as `/items` or `/items/0/name`). As a table: an array of objects becomes a header row of their keys and a row per object, an array of arrays a row per array, an object a key/value row per member (keys in alphabetical order), and a single value one cell; strings are cells as they are, other values their JSON text. As text: the part pretty-printed (strings as they are).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Dat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `json` | JSON | string | `"{\"subject\": \"a lighthouse\", \"style\": \"ink on paper\"}"` |  | The JSON to parse when the input is not connected. |
| `path` | Path | string | `""` |  | A JSON Pointer to the part to output (`/items/0`); empty: all of it. |
| `output` | Output | menu | `"table"` | `table`, `text` | A table (see above) or the part as text. |

## Platforms

All platforms.

## Examples

- [DAT files](../../../examples/dat-files.atidarla/) — `acts`
- [DAT network](../../../examples/dat-network.atidarla/) — `cue`
- [DAT tables](../../../examples/dat-tables.atidarla/) — `parsed`
