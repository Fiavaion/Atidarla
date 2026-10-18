# Compute (`img.compute`)

IMG · version 1 · cook budget 500 µs

A compute shader you write in WGSL: `fn kernel(id: vec2u)` runs once per output pixel. `#bind` lines declare the inputs (`in0`…`in3`), a storage `output` image, read-write `state` images that swap between iterations and persist, named parameters (`p1`…`p8`) and scratch buffers; Tile sets the workgroup size and Iterations how many dispatches run per cook. `time()`, `frame()`, `size()`, `iteration()` and sampler `samp` are available. A file reloads whenever it is saved; the last working shader keeps running while you edit. Reference: docs/compute.md.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in0` | Img | optional |
| `in1` | Img | optional |
| `in2` | Img | optional |
| `in3` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `code` | Code | string | `"#bind input in0\n#bind output dst\n\nfn kernel(id: vec2u) {\n    textureStore(dst, id, textureLoad(in0, id, 0));\n}\n"` |  | The compute shader with its `#bind` lines, used when File is empty. |
| `file` | File | path | `""` |  | Path to a file holding the shader; reloads when it changes. |
| `p1` | P1 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p1()`, or by the name a `#bind param p1` line gives it. |
| `p2` | P2 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p2()`, or by the name a `#bind param p2` line gives it. |
| `p3` | P3 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p3()`, or by the name a `#bind param p3` line gives it. |
| `p4` | P4 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p4()`, or by the name a `#bind param p4` line gives it. |
| `p5` | P5 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p5()`, or by the name a `#bind param p5` line gives it. |
| `p6` | P6 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p6()`, or by the name a `#bind param p6` line gives it. |
| `p7` | P7 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p7()`, or by the name a `#bind param p7` line gives it. |
| `p8` | P8 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p8()`, or by the name a `#bind param p8` line gives it. |
| `tile` | Tile | menu | `"8x8"` | `8x8`, `16x16`, `32x8`, `8x32`, `64x4`, `256x1` | Workgroup size (`TILE_X` × `TILE_Y`): pixels each GPU work group covers. |
| `iterations` | Iterations | int | `1` | 1 – 256 | Dispatches per cook; `state` images swap between them. |
| `reset` | Reset | pulse | `false` |  | Clears the persistent `state` images and buffers. |

## Platforms

All platforms.

## Examples

- [Compute Shaders](../../../examples/compute-shaders.atidarla/) — `blur`, `heat`, `trails`
