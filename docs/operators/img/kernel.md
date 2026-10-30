# Kernel (`img.kernel`)

IMG · version 1 · cook budget 100 µs

A pixel kernel in the Kernel DSL (see the Kernel DSL reference): runs once for every output pixel. `@Cd` starts as input 0 at this pixel and its final value is the pixel; `@P` is the pixel's uv (0..1 from the top-left), `@ix`/`@iy` its column and row, `@res` the output size; `texture(n, uv)` samples input `n` (0 to 3). `ch("name", default)` parameters take their defaults here; a kernel file in the project's `kernels/` folder becomes an operator with controls for them. The last kernel that compiled keeps running while you edit; errors name the line and column.

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
| `code` | Code | string | `"// Runs once for every pixel. @Cd starts as input 0 here; @P is the pixel's uv.\nfloat speed = ch(\"speed\", 1.0, 0.0, 10.0);\nvec3 wave = set(@P.x, @P.y, 0.5 + 0.5 * sin(@Time * speed));\n@Cd = vec4(lerp(@Cd.rgb, wave, 0.5), 1);\n"` |  | The kernel source, used when File is empty. |
| `file` | File | path | `""` |  | Path to a `.atk` file holding the kernel; reloads when it is saved. |

## Platforms

All platforms.

## Examples

- [Kernel Pixels](../../../examples/kernel-pixels.atidarla/) — `pattern`
