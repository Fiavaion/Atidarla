# Piece (`pnt.piece`)

PNT · version 1 · cook budget 40 µs

For-Each Begin: numbers points by piece into a `piece` attribute, so the operators after it act per piece (Randomize keyed by `piece` gives every piece its own value; Group, Delete, Sort and Colour by Attribute read it too): chunks of Size consecutive points, buckets of an attribute's value (for example `copynum` from Copy to Points), or cells of a spatial grid.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Geo | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `rule` | Rule | menu | `"chunks"` | `chunks`, `attribute`, `grid` | chunks: points 0 to Size − 1 are piece 0, and so on; attribute: floor((value − Low) / Width); grid: one number per cell of side Cell (not consecutive). |
| `size` | Size | int | `100` | ≥ 1 | chunks: points per piece. |
| `attribute` | Attribute | string | `"copynum"` |  | attribute: the point attribute read. |
| `component` | Component | int | `0` | 0 – 3 | attribute: which component. |
| `low` | Low | float | `0.0` |  | attribute: the value where piece 0 starts (below it is piece 0 too). |
| `width` | Width | float | `1.0` | ≥ 0.000000001 | attribute: the range of values per piece. |
| `cell` | Cell | float | `0.5` | ≥ 0.000001 | grid: the side of a cell. |

## Platforms

All platforms.

## Examples

- [PNT For-Each](../../../examples/pnt-foreach.lasair/) — `bands`
- [PNT Solver](../../../examples/pnt-solver.lasair/) — `cells`
