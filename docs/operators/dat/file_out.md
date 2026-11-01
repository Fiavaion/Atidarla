# File Out (`dat.file_out`)

DAT · version 1 · cook budget 30 µs

Writes its input to a file on a worker thread, whenever the input changes or on the Write pulse: text as it is, a table as CSV or TSV. Append adds each value to the end of the file (a log). Relative paths are relative to the project folder; missing folders are created. Outputs its input unchanged.

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

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
| `file` | File | path | `""` |  | The file to write. |
| `format` | Format | menu | `"auto"` | `auto`, `text`, `csv`, `tsv` | `auto`: CSV for `.csv`, TSV for `.tsv`/`.tab`, otherwise text (a table as TSV). |
| `mode` | Write | menu | `"on_change"` | `on_change`, `on_pulse` | `on_change` writes each new input value; `on_pulse` only on the Write Now pulse. |
| `write` | Write Now | pulse | `false` |  | Writes the current input now. |
| `append` | Append | toggle | `false` |  | Adds to the end of the file (each value on its own line) instead of replacing it. |

## Platforms

All platforms.

## Examples

- [DAT files](../../../examples/dat-files.lasair/) — `export`
