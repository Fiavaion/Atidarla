# VAT In: how the files are read

`msh.vat_in` (FR-C2) plays vertex animation textures laid out as Houdini's Vertex Animation
Textures 3.0 export describes them publicly: a mesh, a position texture, optional rotation,
colour and lookup textures, and a JSON data file. This page states exactly what the operator
reads, so an export can be checked against it. It was written from the public format
description and tested only on synthetic files (`crates/atd-ops-msh/tests/vat.rs`); no
third-party sample files are used or shipped.

## Where it runs

On an MSH worker thread, like every MSH operator (CLAUDE.md rule 1). Files are decoded once
and kept (by path, modification time and size; the last 12), so a frame change only rebuilds
the frame's mesh. The result is an ordinary GEO value, so PNT deformers and `img.render` read
it directly. A GPU playback path (sampling the textures in a PNT kernel) would skip the CPU
rebuild for very large soft or rigid sets; it is not needed for M6 and would not cover fluid
mode, whose topology changes each frame.

## Files

| Parameter | Formats | Read as |
|---|---|---|
| Mesh | `.obj`, `.ply`, `.gltf`, `.glb` | The rest mesh. OBJ points keep the file's `v` order. |
| Position | `.exr` (values as stored), `.png` 8 or 16 bit (0–1 across the bounds) | RGB = position |
| Rotation | `.exr` (as stored), `.png` (`q = 2c − 1`) | RGBA = quaternion x, y, z, w (normalised) |
| Colour | `.exr`, `.png` | RGBA → `Cd` (point colour, RGBA) |
| Lookup | `.exr` or 16-bit `.png` | Fluid only; RG = position texel, A < 0.5 = unused |
| Data | JSON | frame count, bounds (below) |

FBX meshes are not read: export the VAT geometry as glTF (or OBJ/PLY) for Atidarla.

### Data file

An object, or an array whose first object holds the values. Keys are matched ignoring case,
spaces and underscores:

- frame count: `_frameCount`, `Frame Count`, `frames`, `numOfFrames`;
- bounds: `_boundingMin{X,Y,Z}` / `_boundingMax{X,Y,Z}`, or `Bound Min` / `Bound Max` (also
  `posMin` / `posMax`) as three numbers or an array;
- frame rate: `_houdiniFPS` (read but not applied: drive Frame with an expression).

A PNG position texture needs the bounds; an EXR one does not.

## Texels

Each point finds its texel in frame 0:

1. from the point attribute `uv2` (glTF `TEXCOORD_1`), in Atidarla's `uv` convention (v up):
   column `⌊u·W⌋`, row `⌊(1 − v)·H⌋` with row 0 at the top of the image;
2. else by order: soft and fluid by point number `i`, rigid and sprite by piece number `k`
   (the `piece` point attribute, else connected pieces numbered in primitive order), at column
   `i mod W`, row `⌊i / W⌋`.

Frames are stacked downward: with `R` the number of rows frame 0 uses (the largest row found
plus one), frame `f` is `R·f` rows lower. The frame count comes from the Frames parameter, else
the data file, else `⌊H / R⌋`; a texture too short for it is an error. Padding below the last
frame is ignored.

## Modes

- **soft:** the point moves to the texel's position (Positions: absolute), or by it (offset).
  Normals are recomputed from the surface.
- **rigid:** the texel holds the piece's pivot position and rotation; each point moves to
  `pivot(f) + rotate(q(f), P_rest − pivot_rest)`, where `pivot_rest` is the point attribute
  `pivot` if present, else the piece's centre. `N` is turned with it. Without a rotation
  texture pieces only move.
- **fluid:** like soft, without blending between frames; a point whose lookup texel is unused,
  and every triangle that collapses (zero area), leaves the frame.
- **sprite:** each particle quad keeps its rest shape around its centre, moved to the texel's
  position (camera facing is the renderer's job).

Between frames (Interpolate), positions and colours blend linearly and quaternions by
normalised linear blend on the short path. With Loop, frame `F` is frame 0 again; without it
frames past the end hold the last one.
