# Script (`dat.script`)

DAT · version 1 · cook budget 50 µs

Python that turns DAT inputs into a DAT value: the code defines `cook(inputs)`, where `inputs` has one entry per input (`None`, a `str` for text, a list of rows of `str` for a table), and returns a `str` (text), a list of rows or of values (a table), a dict (name/value rows) or `None` (empty). It runs on a worker thread whenever the code or an input changes, never on the frame (offline renders wait for it). Script is project code: it runs only once you allow the project's Python (the app's Allow dialog; `--allow-python-ops` for `check` and `render`).

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in0` | Dat | optional |
| `in1` | Dat | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `code` | Code | string | `"def cook(inputs):\n    # inputs[0], inputs[1]: None, a str (text) or a list of rows (a table).\n    table = inputs[0] or []\n    return [[cell.upper() for cell in row] for row in table]\n"` |  | Python defining `cook(inputs)`; the rest of the file runs once per edit. |

## Platforms

All platforms.

## Examples

- [DAT script](../../../examples/dat-script.atidarla/) — `shout`, `stats`
