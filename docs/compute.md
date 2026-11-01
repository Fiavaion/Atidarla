# Compute shaders (`img.compute`, FR-K5)

The Compute operator runs a WGSL compute shader you write. You write one function,
`fn kernel(id: vec2u)`, which runs once for every pixel of the output (`id` is the pixel, from
the top-left). `#bind` lines say which images, parameters and buffers the shader uses; the
operator writes the WGSL declarations for them and the entry point around your kernel.

It sits beside WGSL (`img.wgsl`, a full-screen fragment shader), which is unchanged. Use WGSL for
"one colour per pixel from the inputs"; use Compute when the shader iterates, keeps state from
frame to frame, writes several images, or needs a scratch buffer.

```wgsl
#bind input in0
#bind state heat rgba32float from=in0
#bind param p1 rate f32

// One diffusion step per iteration.
fn kernel(id: vec2u) {
    let p = vec2i(id);
    let last = vec2i(size()) - 1;
    let c = textureLoad(heat, p, 0);
    let n = textureLoad(heat, clamp(p - vec2i(1, 0), vec2i(0), last), 0)
          + textureLoad(heat, clamp(p + vec2i(1, 0), vec2i(0), last), 0)
          + textureLoad(heat, clamp(p - vec2i(0, 1), vec2i(0), last), 0)
          + textureLoad(heat, clamp(p + vec2i(0, 1), vec2i(0), last), 0);
    textureStore(heat_out, id, c + rate() * (n - 4.0 * c));
}
```

With Iterations at 8 this runs the step 8 times per cook, swapping `heat` and `heat_out`
between steps, and shows the result.

## Directives

One directive per line, with `#` as the first character on the line (after spaces). Directive
lines become blank lines in the generated code, so error line numbers are the lines of your code.

| Directive | Declares | Notes |
|---|---|---|
| `#bind input inN` | `inN: texture_2d<f32>` | `N` is 0 to 3, the node's input sockets. Unconnected inputs are a 1×1 transparent black image. Read with `textureLoad(inN, p, 0)` or `textureSampleLevel(inN, samp, uv, 0.0)`. |
| `#bind output NAME [FORMAT]` | `NAME: texture_storage_2d<FORMAT, write>` | At most one. When present, it is the node's image. Write with `textureStore(NAME, id, colour)`. |
| `#bind state NAME [FORMAT] [from=inN]` | `NAME: texture_2d<f32>` (read) and `NAME_out: texture_storage_2d<FORMAT, write>` | A pair of images that swap after every iteration: read the previous one with `textureLoad(NAME, p, 0)`, write the next with `textureStore(NAME_out, id, v)`. Without `from=` it persists from frame to frame, starting at zero, and Reset clears it. With `from=inN` it is loaded from that input at the start of every cook (the input is bound for you). If there is no `output`, the first state is the node's image. |
| `#bind param pN NAME [TYPE]` | `fn NAME() -> TYPE` | Names parameter PN (1 to 8) in the shader. `TYPE` is `f32` (the first component), `vec2f`, `vec3f` or `vec4f` (default). The node's status line lists the names. `p1()` to `p8()` (each a `vec4f`) always work too. |
| `#bind buffer NAME COUNT [TYPE]` | `var<storage, read_write> NAME: array<TYPE, COUNT>` | A scratch buffer that persists from frame to frame, starting at zero; Reset clears it. `COUNT` is a number or `pixels` (one element per output pixel, as a runtime-sized array; index it `id.y * size().x + id.x`). `TYPE` is `f32` (default), `u32`, `i32`, `vec2f`, `vec4f`, `atomic_u32` or `atomic_i32`. |

`FORMAT` is one of `rgba8unorm`, `rgba16float` (default), `rgba32float` or `r32float`. These
are the storage formats wgpu guarantees for write-only storage on every backend without
optional features (WebGPU core), so a shader that compiles on one machine compiles on DX12,
Metal and Vulkan alike. `rgba32float` and `r32float` cannot be read through the filtering
sampler `samp`; read them with `textureLoad`. `r32float` shows as red.

Names must be WGSL identifiers, unique, and not one of the generated names (`lsr`, `lsr_*`,
`samp`, `kernel`, `time`, `frame`, `size`, `iteration`, `iterations`, `TILE_X`, `TILE_Y`,
`LsrParams`, `in0` to `in3`, `p1` to `p8`).

**Limits (all within what every adapter guarantees, `wgpu::Limits::downlevel_defaults`):** at
most 4 output and state images together (`max_storage_textures_per_shader_stage` is 4), at most
4 buffers (`max_storage_buffers_per_shader_stage` is 4 at the downlevel floor), a buffer no
larger than the device's `max_storage_buffer_binding_size` (128 MiB by default), at most 256
iterations. The limits test in `crates/lsr-ops-img/src/compute.rs` checks these numbers and
the format guarantees against wgpu's tables; `tests/compute.rs` runs every format on the real
device.

## Built-ins

| Name | Value |
|---|---|
| `size() -> vec2u` | The output size in pixels. The kernel only runs for `id < size()`. |
| `time() -> f32` | Seconds (the engine's time; the frame's time in an offline render). |
| `frame() -> u32` | The engine's frame number. |
| `iteration() -> u32`, `iterations() -> u32` | This dispatch's index (0 first) and how many run this cook. |
| `p1()` … `p8() -> vec4f` | The parameters P1 to P8. |
| `samp` | A linear-filtering sampler (clamp to edge). |
| `TILE_X`, `TILE_Y` | The workgroup size as constants, for `var<workgroup>` arrays. |

## Parameters

- **Code / File**: the shader inline, or a file in the project that reloads when saved.
- **Tile**: the workgroup size, `8x8` (default) to `256x1`; each is at most 256 invocations. The
  image does not depend on it (tested); speed may.
- **Iterations**: dispatches per cook, 1 to 256. Each gets its own `iteration()`.
- **Reset**: clears persistent states and buffers (a pulse: any change of its value).

## Errors and live editing

Compilation runs on a worker thread: directives are checked first, then naga validates the
module, then the driver compiles it. Errors name your line and column (`line 4, col 39: ...`);
validation errors give the whole chain of causes. While a new version compiles, and when it
fails, the last program that compiled keeps running (when it fails, the node shows the error and
keeps its last image, as WGSL does). If the new program has the same images and buffers, they
are kept, so editing the code does not lose the state.

Offline (`lasair render`) the cook waits for the compiler, and a file is compiled once, as it
was when the render started (`docs/render.md`). Compute is in the "waits" class there; its
persistent states and buffers also make it stateful (frames of a render started part-way differ,
as for Feedback).

## How it runs

Per cook: one uniform block per iteration (256-byte stride, dynamic offset: P1-P8, time, frame,
iteration, size); a compute pass with the `from=` loads and then the iterations, swapping the
state pairs; then a copy of the shown image into the engine's output through a small render pass
(the engine's textures have no storage use, and the shown image may be any of the four formats).
Bind groups are cached per set of input textures, so steady-state frames create no GPU objects
(rule 2; `tests/alloc.rs`).

## Not yet

- Your own entry point (`@compute fn main` with workgroup barriers and shared memory over a
  tile): only `fn kernel` is supported now. `TILE_X`/`TILE_Y` are there for when it is.
- A "Generated WGSL" view in the app (the generated module is `compute::generate`).
- Per-node labels for named parameters: the parameter panel still says P1 to P8 (the names show
  in the status line), because operator schemas are per type.
- Geometry buffers: see `docs/milestones/m2/nca-compute.md` for how `#bind` extends to PNT/MSH
  attributes.
