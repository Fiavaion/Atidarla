# Kernel (`pnt.kernel`)

PNT · version 1 · cook budget 150 µs

A point kernel in the Kernel DSL (VEX-flavoured, see the Kernel DSL reference): runs once for every point, reading and writing attributes as `@name` (`v@v`, `f@age`, …; missing ones are created at zero), with `@Time`, `@TimeInc`, `@Frame`, noise, `point()` and `nearpoints()`. `ch("name", default)` parameters take their defaults here; a kernel file in the project's `kernels/` folder becomes an operator with controls for them. Put it inside a Solver loop and its attributes persist from frame to frame; editing the code keeps the simulation running. The last kernel that compiled keeps running while you edit; errors name the line and column.

Cooks every frame (time-dependent).

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
| `code` | Code | string | `"// Runs once for every point. Read and write attributes with @name.\n@P.y += sin(@P.x * 3.0 + @Time * 2.0) * 0.5 * @TimeInc;\n"` |  | The kernel source, used when File is empty. |
| `file` | File | path | `""` |  | Path to a `.atk` file holding the kernel; reloads when it is saved. |

## Platforms

All platforms.

## Examples

- [Kernel Particles](../../../examples/kernel-particles.atidarla/) — `ripple`
