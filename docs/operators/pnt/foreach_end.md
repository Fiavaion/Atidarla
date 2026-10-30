# For-Each End (`pnt.foreach_end`)

PNT · version 1 · cook budget 150 µs

For-Each End: merges the results of a For-Each block's passes into one geometry, pass 0's points first. Attributes are those of the first pass with points (zero where a later pass lacks one); primitives and groups are not carried. Wire its output back into the For-Each's `end` input to close the block; outside a block it passes its input through.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

None.

## Platforms

All platforms.

## Examples

- [PNT For-Each](../../../examples/pnt-foreach.atidarla/) — `merge_copies`, `merge_bands`
