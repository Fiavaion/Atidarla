# Merge (`pnt.merge`)

PNT · version 1 · cook budget 150 µs

Combines up to four geometries into one: their points one after the other (a, then b, c, d). Attributes are the union of the inputs'; points from an input without one get zero (an attribute whose type differs from the first input's is treated as missing). Groups are not carried; primitives are dropped.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `a` | Geo | optional |
| `b` | Geo | optional |
| `c` | Geo | optional |
| `d` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

None.

## Platforms

All platforms.

## Examples

No example project uses it yet.
