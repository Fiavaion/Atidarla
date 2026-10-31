# File In (`dat.file_in`)

DAT · version 1 · cook budget 30 µs

Reads a text file (up to 64 MB) on a worker thread: as text, or as a table from CSV or TSV. With Watch on it reloads whenever the file is saved; Reload reads it again now. Relative paths are relative to the project folder. Offline renders read it before the first frame.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `file` | File | path | `""` |  | The file to read. |
| `format` | Format | menu | `"auto"` | `auto`, `text`, `csv`, `tsv` | `auto`: a table for `.csv` and `.tsv`/`.tab` files, text for anything else (feed `.json` to JSON). |
| `watch` | Watch | toggle | `true` |  | Reloads the file when it changes on disk. |
| `reload` | Reload | pulse | `false` |  | Reads the file again now. |

## Platforms

All platforms.

## Examples

- [DAT files](../../../examples/dat-files.atidarla/) — `cues`, `show`
