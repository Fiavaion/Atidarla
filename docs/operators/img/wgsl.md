# WGSL (`img.wgsl`)

IMG · version 1 · cook budget 100 µs

A full-screen shader you write in WGSL: the body of `fn shade(uv: vec2f) -> vec4f` (uv from the top-left). Inputs are `in0` and `in1` with sampler `samp`; `p1()`…`p4()` return the four parameters, and `time()`, `resolution()` and `frame()` are available. A file reloads whenever it is saved; the last working shader keeps running while you edit.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in0` | Img | optional |
| `in1` | Img | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Img |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `code` | Code | string | `"return textureSample(in0, samp, uv);"` |  | The shader body, used when File is empty. |
| `file` | File | string | `""` |  | Path to a file holding the shader body; reloads when it changes. |
| `p1` | P1 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p1()`. |
| `p2` | P2 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p2()`. |
| `p3` | P3 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p3()`. |
| `p4` | P4 | vec4 | `[0.0,0.0,0.0,0.0]` |  | Returned by `p4()`. |

## Platforms

All platforms.

## Example

Pending: runnable example projects arrive with the project format (PRD A11, Proposed).
