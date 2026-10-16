# Chroma Key (`img.chroma_key`)

IMG · version 1 · cook budget 80 µs

Makes pixels close to a key colour transparent.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `key` | Key | colour | `[0.0,1.0,0.0,1.0]` |  | Colour to remove. |
| `tolerance` | Tolerance | float | `0.3` | ≥ 0 | Colour distance treated as fully keyed. |
| `softness` | Softness | float | `0.1` | ≥ 0 | Extra distance over which the key fades. |

## Platforms

All platforms.

## Examples

- [Keying](../../../examples/keying.atidarla/) — `key`
