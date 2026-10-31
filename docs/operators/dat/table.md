# Table (`dat.table`)

DAT · version 1 · cook budget 100 µs

A table typed in: one row per line, cells split by the delimiter (CSV-style quotes allowed for every delimiter but tab). Rows shorter than the widest are padded with empty cells. Feeds Select, Prompt Template, Convert and anything that reads a DAT.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `rows` | Rows | string | `"name\tvalue\nsubject\ta lighthouse\nstyle\tink on paper"` |  | The table as text, one row per line. |
| `delimiter` | Delimiter | menu | `"tab"` | `tab`, `comma`, `semicolon`, `pipe`, `space`, `whitespace` | What separates cells: tab, comma, semicolon, pipe, a space, or `whitespace` (any run of spaces and tabs). |

## Platforms

All platforms.

## Examples

- [DAT tables](../../../examples/dat-tables.atidarla/) — `cast`, `words`
