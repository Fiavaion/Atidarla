# VAT In (`msh.vat_in`)

MSH · version 1 · cook budget 100 µs

Plays vertex animation textures in the layout of Houdini VAT 3.0 (soft, rigid, fluid, sprite): the exported mesh moved by its position (and rotation) texture at a frame, with colours. Drive Frame with an expression to play.

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Geo |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `mode` | Mode | menu | `"soft"` | `soft`, `rigid`, `fluid`, `sprite` | The export's mode: soft (one texel per point, fixed topology), rigid (one texel per piece: its position and rotation), fluid (topology changing per frame; unused triangles collapse), sprite (one texel per particle quad). |
| `mesh` | Mesh | path | `""` |  | The exported mesh (.obj, .ply, .gltf, .glb). Points find their texel from `uv2` (glTF TEXCOORD_1), else by their order. |
| `position` | Position Texture | path | `""` |  | The position texture (.exr: positions as they are; .png: 0 to 1 across the data file's bounds). |
| `rotation` | Rotation Texture | path | `""` |  | Rigid only: piece rotations as quaternions (x, y, z, w; .png stores them as 0.5 + q / 2). |
| `colour` | Colour Texture | path | `""` |  | Optional: writes the point colour `Cd` (RGBA) per frame. |
| `lookup` | Lookup Texture | path | `""` |  | Fluid only, optional: per point and frame, where its position is in the position texture (red, green: 0 to 1 across it; alpha below 0.5: unused). |
| `data` | Data File | path | `""` |  | The export's JSON data file: frame count (`_frameCount`) and bounds (`_boundingMin` / `_boundingMax` X, Y, Z). |
| `frame` | Frame | float | `0.0` |  | The frame shown, from 0; drive it with an expression such as `time * 24` to play. |
| `frames` | Frames | int | `0` | ≥ 0 | Number of frames; 0 takes it from the data file, else from the texture height. |
| `positions` | Positions | menu | `"absolute"` | `absolute`, `offset` | Soft and fluid: absolute: the texture holds positions; offset: it holds offsets added to the mesh's points. |
| `loop` | Loop | toggle | `true` |  | Frames past the end start again (else the last frame holds). |
| `interpolate` | Interpolate | toggle | `true` |  | Blends between neighbouring frames (not in fluid mode, whose topology changes). |

## Platforms

All platforms.

## Examples

- [VAT Playback](../../../examples/mesh-vat.lasair/) — `ripple`, `tumble`
