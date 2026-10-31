# Convert (`dat.convert`)

DAT · version 1 · cook budget 200 µs

Converts its input between text, table and JSON: text to a table (split by the delimiter), a table to delimited text (CSV-style quoting where needed), or either to JSON (rows as objects keyed by the header row, or as arrays). A value already in the wanted form passes through unchanged.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Dat |  |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `to` | To | menu | `"table"` | `table`, `text`, `json` | The form to convert to. |
| `delimiter` | Delimiter | menu | `"tab"` | `tab`, `comma`, `semicolon`, `pipe`, `space`, `whitespace` | The cell delimiter for text: splitting text into a table, or joining a table into text. |
| `header` | Header Row | toggle | `true` |  | To JSON: the first row names the columns, and every other row becomes an object. Off: every row becomes an array. |
| `trim` | Trim | toggle | `false` |  | Removes spaces around every cell of a table made from text. |

## Platforms

All platforms.

## Examples

- [DAT tables](../../../examples/dat-tables.atidarla/) — `as_json`, `csv_table`
- [Speech and chat](../../../examples/speech-chat.atidarla/) — `answer`
