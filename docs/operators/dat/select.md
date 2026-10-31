# Select (`dat.select`)

DAT · version 1 · cook budget 300 µs

Keeps some rows and columns of its input table (text counts as one row per line). Rows are filtered by a match on one column first, then picked by index; columns are picked by name or index. With Header Row on, the first row names the columns, is not counted and is always kept.

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
| `header` | Header Row | toggle | `true` |  | The first row names the columns: it is kept, and row indices start after it. |
| `rows` | Rows | string | `""` |  | Rows to keep, counted from 0: indices and ranges separated by commas (`0, 2-4, 7-`). Empty: all. |
| `cols` | Columns | string | `""` |  | Columns to keep, in this order: names (with Header Row on) or indices and ranges (`name, 2, 4-`). Empty: all. |
| `match_col` | Match Column | string | `""` |  | The column (name or index) whose cells must match `Match`. Empty: no filter. |
| `match` | Match | string | `"*"` |  | Pattern a row's cell must match to be kept: `*` is any text, `?` any one character; letter case is ignored. |

## Platforms

All platforms.

## Examples

- [DAT files](../../../examples/dat-files.atidarla/) — `verses`
- [DAT serial](../../../examples/dat-serial.atidarla/) — `readings`
- [DAT tables](../../../examples/dat-tables.atidarla/) — `lights`
