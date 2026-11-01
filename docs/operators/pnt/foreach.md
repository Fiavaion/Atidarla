# For-Each (`pnt.foreach`)

PNT · version 1 · cook budget 60 µs

For-Each Begin: the operators between it and a For-Each End run once per pass, and End merges the passes' results. Count: every pass gets the whole input; pieces: pass k gets the points whose piece attribute is k (from Piece). Each pass's points carry `iteration` (its number, also a detail value), so operators keyed by an attribute (Randomize, Attribute Math, Colour by Attribute) differ per pass. Wire End's output into `end` to close the block.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |
| `end` | Geo | feedback: reads the source's previous frame; optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `mode` | Mode | menu | `"count"` | `count`, `pieces` | count: every pass gets the whole input; pieces: pass k gets the points whose Piece Attribute is k (pieces numbered from Iterations up are left out). |
| `iterations` | Iterations | int | `4` | 1 – 256 | How many passes (count), or pieces numbered 0 to this − 1 (pieces). The body costs this many times its single cost. |
| `attribute` | Piece Attribute | string | `"piece"` |  | pieces: the one-component point attribute that numbers the pieces (Piece writes `piece`; chunks or attribute buckets number them from 0). |

## Platforms

All platforms.

## Examples

- [PNT For-Each](../../../examples/pnt-foreach.lasair/) — `copies`, `per_band`
