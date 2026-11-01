# Prompt Template (`dat.prompt_template`)

DAT · version 1 · cook budget 200 µs

Fills `{name}` placeholders in its template: from the `values` table (a name and a value per row, or a header row of names over rows of values), from channels on the `sig` input (formatted to `Precision` decimals), and `{text}` from the `text` input (a Speech to Text transcript, say). `{{` and `}}` write braces. Outputs text for LLM Chat, a diffusion prompt or a Web Client body; it changes only when the filled text does.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `values` | Dat | optional |
| `text` | Dat | optional |
| `sig` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `template` | Template | string | `"Describe {subject} in the style of {style}. {text}"` |  | The text, with `{name}` where a value goes. |
| `layout` | Layout | menu | `"pairs"` | `pairs`, `columns` | How the `values` table holds its values: `pairs` is a name and a value per row; `columns` is a header row of names over rows of values (see `Row`). |
| `row` | Row | int | `1` | ≥ 1 | With `columns`: the row of values to use (1 is the first after the header). |
| `missing` | Missing | menu | `"keep"` | `keep`, `empty`, `error` | A placeholder with no value: kept as written, left empty, or a node error. |
| `precision` | Precision | int | `3` | 0 – 9 | Decimals of channel values. |

## Platforms

All platforms.

## Examples

- [DAT tables](../../../examples/dat-tables.lasair/) — `prompt`
