# Point File In (`pnt.file_in`)

PNT · version 1 · cook budget 100 µs

Reads a point cloud from a file: Atidarla's point cache (.atdpc), PLY (ASCII or binary; the vertex element), CSV with a header row, or XYZ/PTS text. `x y z` become `P`, `nx ny nz` `N`, colour channels `Cd` (0-1; 8- and 16-bit PLY colours are scaled), `u v` `uv`, `id` stays the point identity and every other column becomes a float attribute (at most 16). A `$F` in the file name (`$F4`: four digits, zero-padded) plays a numbered sequence by Frame. Files are read and uploaded on a worker thread; live, the last file stays until the next arrives. Convert other caches with `atidarla points convert` (docs/point-caches.md).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `file` | File | path | `""` |  | The point file, or a sequence with `$F` / `$F4` for the frame number (relative paths are relative to the project folder). |
| `frame` | Frame | int | `1` |  | The frame `$F` stands for; drive it with an expression such as `frame + 1` to play a sequence. |

## Platforms

All platforms.

## Examples

- [PNT Capture](../../../examples/pnt-capture.atidarla/) — `spiral`, `lattice`, `pulse`
