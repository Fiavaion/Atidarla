# M2 pre-work: Kernel DSL compiler (`atd-kernel`)

> **A12 ratified 2026-10-30.** The compiler is wired into the engine as `pnt.kernel`
> (2026-10-30, see the M2 pre-work log and decision A53), and then as `img.kernel` and
> project kernel operators (`kernels/<name>.atk` → `kernel.<name>`, `ch()` parameters as
> controls; 2026-10-30, "Engine integration, second slice" below). The questions below were
> answered "as proposed"; A53 lists the few points decided differently while integrating.

Status: compile side of FR-K1..K3 done and tested, 2026-10-18. Engine integration: built,
2026-10-30 (`pnt.kernel`, `img.kernel`, project kernel operators, FR-K4 hot reload); the
plan below is kept as written, with what was built noted under each part. Still not built:
image inputs in Points kernels, prims and voxels, a spatial grid for `nearpoint(s)`.

## Engine integration, second slice (2026-10-30): `ch()` controls and Pixels

| Requirement | Built | Tests (all passing) |
|---|---|---|
| FR-K1 `ch()` hooks as controls | `crates/atd-ops-pnt/src/project.rs` `ProjectKernels`: each `kernels/<name>.atk` / `<name>.pixels.atk` registers `kernel.<name>` (`KERNEL_CUSTOM` provenance; `Registry::replace_custom` accepts `kernel.<name>`); parameters from `Compiled::params` (`ch`/`chf` float, `chi` int, `chu`/`chv`/`chp` vectors; defaults, min, max; help from a `//` comment on the line); compiled at project open and on the watcher thread on save; `stale_edits` (now `atd_core::graph::stale_edits`, by provenance) keeps values whose name and kind fit | `tests/project_kernels.rs`: `kernel_files_register_operators` (schema shape, kinds, defaults, help, bad names, duplicates, line of a compile error, `explain`), `saving_the_file_updates_the_schema_and_keeps_matching_values`, `kernel_nodes_round_trip_in_project_files`; unit tests in `project.rs` |
| Values reach the GPU | One runner per target (`PointsRunner` in `atd-ops-pnt/src/kernel.rs`, `PixelsRunner` in `atd-ops-img/src/kernel.rs`) taking a source and a value provider (`ChValues::Defaults` for the inline operators, `ChValues::Params` for project operators: `Params` with bindings, expressions, live values); `write_uniforms` by `ParamDecl::slot` into buffers allocated at compile | `editing_a_ch_value_changes_the_gpu_result` (constants, an expression, Points and Pixels); `tests/alloc_kernel.rs` (no buffers, bind groups or textures after warm-up; 57.5 allocations per node-cook on Metal, bound 90) |
| FR-K4 for project operators | A node reads its file's latest text through a `SharedSource` and compiles on its own worker; the last kernel that compiled keeps running | `saving_a_running_kernel_keeps_its_state` (Solver loop; y kept, x continues at the kept `speed`) |
| `img.kernel` (Pixels) | Full-screen draw from the compiled module through the new `FullscreenPass::try_from_module` (`atd-gpu`: group 0 uniforms + sampler, group 1 inputs) and `draw_bytes`; inputs `in0`…`in3`, unconnected ones black; `res` = output size | `atd-ops-img/tests/kernel.rs`: pixels against the CPU (uv, indices, resolution, time, an input and an unconnected one), a bad edit names `kernel:2:` and the last kernel keeps drawing |
| Examples | `kernel-particles` uses `kernel.curl` (its `ch()`s as controls) and an inline `pnt.kernel`; new `kernel-pixels` (inline `img.kernel` generator → `kernel.grade` from `kernels/grade.pixels.atk`) | `atd-cli` `kernel_operators_render_deterministically`; `atidarla render … --range 0-60` twice: 60/60 identical for both |

Not decided here: `ch()` has no colour or toggle form (the docs say to use `chp` and
`chi(name, 0, 0, 1)`); adding `chc`/`cht` would be a language change for A12/A53.

## Plan and evidence: FR-K1..K3 (compile side) → tasks → tests

| Requirement | Built | Tests (all passing) |
|---|---|---|
| FR-K1 `@attr` with schema-generated bindings, `ch()` hooks | Lexer, parser, typed IR; attribute typing from sigil, input schema or standard names; `Bindings`, `AttrBinding`, `ParamDecl`, uniform layout | 8 golden WGSL files; binding layout test (sorted by name, 16-byte stride for `vec3`, snapshot bindings); schema tests; uniform packing test |
| FR-K1 targets | Points (compute) and Pixels (full-screen fragment). Prims and voxels: not built | Goldens for both targets |
| FR-K2 helpers | `point()`, `npoints()`, `nearpoint()`, `foreach … nearpoints()` (brute force), perlin/simplex/worley (2D and 3D), curl noise, `rand()`, maths, matrices, quaternions, `texture()` | GPU vs Rust noise on a 24³ grid; GPU vs interpreter on five kernels |
| FR-K3 inline errors, Generated WGSL | `Diagnostic { line, col, len, message, hint, span, origin }`, `Diagnostic::render`; naga validation of every shader with errors mapped back through a `SourceMap`; `Compiled::wgsl` is the "Generated WGSL" text | 51 positioned error cases; cascade suppression; deterministic fuzz (never panics, never emits WGSL naga rejects); 1 MiB-stack test |
| Determinism | Pure lowering, fixed helper order, no hidden time or randomness, parameters as uniforms | Same source (LF or CRLF, any parameter values) → byte-identical WGSL; curl kernel byte-identical over two 120-step GPU runs |
| AC2 groundwork | The ten-line curl-noise kernel, written out in `docs/kernel-dsl.md` | Extracted from the docs, compiled, validated, run on GPU and CPU |
| CLI | `atidarla kernel <file> [--target points\|pixels] [--emit-wgsl]` | `atd-cli` unit test; run by hand on the goldens and a bad file |

Evidence, run on 2026-10-18, Windows 11, RTX 5070 Ti, DX12, Rust 1.97, debug build unless noted:

- `cargo test -p atd-kernel`: 39 tests, 0 failed (18 unit, 2 docs, 4 edges, 6 errors, 3 fuzz,
  2 golden, 3 GPU, 1 doc-test); `cargo test -p atd-cli kernel`: 1 test.
- The AC2 kernel in `docs/kernel-dsl.md` is asserted identical to the golden
  `curl_particles.points.atk` that the GPU tests run.
- GPU noise vs Rust: largest |gpu − cpu| perlin3 1.7e-6, perlin2 4.8e-7, simplex3 2.5e-6,
  simplex2 2.4e-6, worley3 6.0e-8, worley2 6.0e-8, curl 3.7e-6; `rand()` bit-exact.
- GPU vs interpreter, largest float error (integers exact): curl particles 1000 points × 60
  steps 6.4e-6; control flow 5.9e-5; matrices 2.9e-6; noise 2.3e-6; neighbours (snapshot
  bindings, `nearpoints`) 300 points × 3 steps 1.2e-7.
- Fuzz: 3,000 mutants per run by default; 200,000 once in release (clean). It found one real
  bug (a `point()` name that is not an identifier produced invalid WGSL), fixed.
- The GPU parity test found that Gustavson's 3D simplex with kernel radius 0.6 is
  discontinuous at simplex edges (one ulp of difference in corner order became a 7e-4 jump);
  radius 0.5 is continuous and is what ships.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all` clean;
  `cargo test --workspace`: 335 passed, 0 failed, 1 ignored.

## Public API (what other areas call)

```rust
use atd_kernel::{compile, CompileOptions, Target, AttrSchema, AttrType, Scalar};

let mut opts = CompileOptions::new(Target::Points);  // validate: true, max_storage_buffers: 8
opts.input_schema = Some(vec![AttrSchema { name: "P".into(), ty: AttrType::VEC3 }]);
match compile(source, &opts) {                       // pure, may take ms: call off the render thread
    Ok(c) => { /* c.wgsl, c.bindings, c.params, c.source_map, c.state_keys() */ }
    Err(diagnostics) => { /* Vec<Diagnostic>: line, col, len, message, hint, span, origin */ }
}
```

| Item | Purpose |
|---|---|
| `compile(&str, &CompileOptions) -> Result<Compiled, Vec<Diagnostic>>` | Never panics (internal panics become a diagnostic) |
| `Compiled { target, wgsl, bindings, params, source_map, program }` | `wgsl` is the complete module and the "Generated WGSL" view |
| `Compiled::write_uniforms(&FrameGlobals, &[[f32; 4]], &mut [u8])` | Fills the uniform buffer without allocating (`values[slot]`) |
| `Compiled::state_keys() -> Vec<(String, AttrType)>` | FR-K4 reload key: written attributes by name and storage type |
| `Bindings { workgroup_size, entry_point, vertex_entry_point, header, uniforms, uniform_size, sampler, attributes, images }` | Everything needed to build bind group layouts (the GPU test builds them from this alone) |
| `AttrBinding { name, ty, kernel_type, access, snapshot, created_if_missing, group, binding, stride, wgsl_name }` | One storage buffer |
| `ParamDecl { name, ty, default, min, max, slot, span }` | One `ch()` parameter, for the UI |
| `Diagnostic::render(file, source)` | `file:line:col: error: …` with the line and a caret underline |
| `interp::{Geometry, Attribute, run_points}` | CPU reference interpreter (tests, not the frame path) |
| `noise::*`, `noise_library() -> String` | Rust noise functions; the same noise as standalone WGSL for node-built operators (AC2) |
| `bindings::{WORKGROUP_SIZE, HEADER_SIZE, PARAMS_OFFSET, MAX_PARAMS}` | Layout constants (64, 8 bytes, 32 bytes, 64) |

### Binding layout (follows `docs/design/geometry-attributes.md` §2.1 and §5)

| | Group 0 (engine) | Group 1 | Group 2 |
|---|---|---|---|
| Points | 0: header `{count: u32, capacity: u32}` (storage, read); 1: uniforms; 2: sampler (only when images are used) | attributes, sorted by name; `read` or `read_write`; a snapshot (read) right after an attribute that is written and also read at other points | images, binding = input index |
| Pixels | 0: uniforms; 1: sampler | images, binding = input index (input 0 always bound) | — |

Uniforms: `globals: vec4f` (time, time step, frame, 0), `res: vec4f` (width, height, 1/width,
1/height), then one `vec4f` per `ch()` parameter (`int` as `i32` bits). Points dispatch
`ceil(count / 64)` workgroups as `(x, y, 1)` with `x ≤ 65535`; the kernel's index is
`gid.x + gid.y * num_workgroups.x * 64` and it returns when the index ≥ `header.count`.
Attribute buffers use the attribute's storage type at its stride (16 bytes for `vec3`).

## Integration plan (not built: the geometry area owns GEO)

### `pnt.kernel` (Points)

1. **Schema.** Inputs: `geo` (GEO), `img0`…`img3` (IMG, optional). Output: GEO. Params:
   `code` (multiline string), `file` (path, reloads on save), plus the `ch()` parameters of
   the current compile (see "What the engine must provide", 2).
2. **Compile off the render thread** (rule 1), as `img.wgsl` does: a worker thread calls
   `compile` with `input_schema` taken from the upstream GEO value's attribute list (design
   §5.1), then creates the shader module and the compute pipeline with explicit layouts built
   from `Bindings` (as `tests/gpu.rs` does). Recompile when the code, the file or the input
   schema (names and types only) changes. The last pipeline that compiled keeps running;
   errors go to the node as `Diagnostic`s for inline display. Offline (`CookMode::Offline`)
   the cook waits for the compile with `CookMode::receive`, as `img.wgsl` does.
3. **Cook (render thread, no allocation after warm-up, rule 2).** For each `AttrBinding`:
   `Read` binds the input's buffer; `ReadWrite` binds a buffer the node holds uniquely (the
   design's ownership hand-over, else copy-on-write from the buffer pool);
   `created_if_missing` binds a zero-filled pooled buffer when the input lacks the attribute;
   a `snapshot` binds the unchanged input buffer and therefore forces copy-on-write for its
   `ReadWrite` twin (they must not alias). The header is the engine's GPU count buffer (design
   §4.2). `write_uniforms` writes into a buffer allocated once; values come from the node's
   parameters by name (`ParamDecl::slot`). Bind groups are cached by buffer identity, as
   `FullscreenPass` does.
4. **Dispatch.** Direct: `(x, y, 1)` from the CPU-side count. With a GPU-side count (design
   §4.2), `dispatch_workgroups_indirect` with `(ceil(count/64), 1, 1)` works up to 4.19M
   points; beyond that a one-thread pass must write `(x, y, 1)` (the kernel already supports
   the 2D index).

### `img.kernel` (Pixels)

A full-screen draw: the module has `vs` and `fs`; group 0 holds the uniforms (with `res` =
output size) and a linear clamp-to-edge sampler, group 1 the input images by index (input 0
always; unconnected inputs bind the engine's black texture, as `img.wgsl` does). The cleanest
route is a `FullscreenPass` constructor that takes a complete module and a layout. *Built
2026-10-30 as planned: `FullscreenPass::try_from_module` with `ModuleLayout`.*

### State-preserving hot reload (FR-K4, AC3)

- Simulation state lives on the GEO wire (design D6, §3.2, §5.4), not in the operator. An
  edit recompiles on the worker; the new pipeline is swapped in at the start of a later cook,
  and the cook carries on with the GEO value it receives, so particles keep their state.
- Keys: `Compiled::state_keys()` (written attributes as name + storage type). After a swap, an
  attribute with the same name and type keeps its buffer and values; a changed type gets a new
  zero-filled buffer (and a node warning naming it); an attribute the new kernel no longer
  writes stays on the GEO value untouched; a new one is created zero-filled.
- Parameters are keyed by `ch()` name and type: unchanged ones keep their values, new or
  retyped ones start at their default.
- A failed edit keeps the old pipeline running and shows the diagnostics.
- AC3 test: step N frames, swap the kernel source, step again, and check that `P` continued
  from frame N (not reset) and that a renamed attribute is new and zeroed.

### What the engine must provide

1. The GEO result path, buffer pool, ownership hand-over and per-value attribute schema of
   the geometry design (§2–§5); a GPU count/header buffer.
2. **Parameters created by code**: `ch()` makes parameters at compile time, but
   `OperatorSchema` parameters are static today. Needs a node-level list of extra parameters
   (saved in the project file with stable names, rule 8) or a parameter kind for "kernel
   parameters". This is an engine/format decision (question 8). *Built 2026-10-30 another
   way: a project's kernel file is an operator type of its own (`kernel.<name>`, like a
   Python operator), so its `ch()` parameters are ordinary schema parameters, saved by name;
   the inline operators keep defaults.*
3. Requesting more than 8 storage buffers where the adapter offers them (design §7), passed
   as `CompileOptions::max_storage_buffers`.
4. A home for the shared WGSL library: for AC2 the node-built Curl Noise must call the same
   `atd_curlnoise` as the kernel (`noise_library()`); design §5.5 puts the library in
   `atd-gpu` (question 14).
5. Later: a spatial grid for `nearpoint(s)`, which are O(N) per point today.

## A12-specific design choices: questions for Mark

1. **Surface syntax.** VEX-flavoured C-like syntax: `@attr` with VEX sigils, `ch()` family,
   `let` for inferred locals, Atidarla type names (`vec3`, `mat4`) with VEX aliases (`vector`,
   `matrix`). Accept?
2. **`bool` exists** and comparisons return it (VEX returns `int`); conditions still accept
   `int` and `float`, and `bool` converts to `int` implicitly.
3. **Function parameters are by value** (VEX passes by reference); no recursion; no `@`
   inside functions.
4. **Loops**: only counted `for` loops (`for (int i = A; i < B; i++)`, constant step, counter
   read-only), plus a hard cap of 2²⁰ iterations per loop; no `while`. Enough, or should
   `while` exist with the cap?
5. **No arrays.** `nearpoints()` only as a `foreach` source, visiting points in index order
   (VEX: nearest first), brute force. Arrays (fixed capacity) would allow VEX's form.
6. **Standard attribute types**: `Cd` `vec4` and `uv` `vec2` (the attribute model) instead of
   VEX's `vec3`; `id` stored as `u32`, read as `int`. Without a schema a sigil wins over the
   table; with the engine's schema a disagreeing sigil is an error.
7. **Integer semantics** follow WGSL: wrapping; `x / 0 = x`, `x % 0 = 0`; hex literals up to
   `0xFFFFFFFF` are bit patterns.
8. **Parameter names and storage** (see engine item 2): names are identifiers, slots in order
   of first use; how should they be namespaced against the operator's own parameters
   (`code`, `file`)?
9. **Pixels is a fragment shader**, not compute: it slots into the existing IMG path. FR-K5's
   tile size therefore applies only to a future compute form.
10. **Points dispatch**: 64 invocations per workgroup and the 2D index above.
11. **Noise algorithms are now part of what a project means** (changing them changes every
    render): PCG3D hash, improved-Perlin gradients with quintic fade, simplex radius 0.5 and
    scale 76, Worley F1 with one point per cell, curl of a Perlin potential with fixed
    offsets, `rand()` from PCG3D. Freeze them now, or keep them changeable until 1.0?
12. **File naming**: `.atk` (PRD §5.5 `kernels/*.atk`); the CLI infers the target from
    `*.pixels.atk`. A project would store the target on the node instead.
13. **Experimental flag**: should the operators, once built, be hidden behind a setting
    until A12 is ratified?
14. **Where the shared WGSL library lives**: `atd-kernel` now (so the crate stays removable);
    design §5.5 says `atd-gpu`. Move it when A12 is ratified?
15. **AC2 "identical"**: the kernel and a node chain are different shaders, so FMA
    contraction can differ by an ulp even with the same library; this repeats the geometry
    design's Q4. Accept a tolerance (for example ≤ 1 ulp, or identical PNG frames)?

## Not done, and limits

- Engine and registry integration, `pnt.kernel`/`img.kernel` operators, FR-K4 (by
  instruction: the geometry area owns GEO). FR-K5 (`#bind` raw WGSL) is not in this area.
- Targets: prims and voxels; detail attributes; matrix, string and integer-vector
  attributes; geometry inputs other than 0; arrays.
- `nearpoint(s)` are brute force; fine for thousands of points, not millions.
- The interpreter runs Points only and not `texture()`; the Pixels target is checked by
  naga and the goldens, not run on a GPU in tests.
- GPU tests ran on DX12 (RTX 5070 Ti) only. naga validates the WGSL for every backend, but
  Metal has not been run (rule 6: a Mac run is needed before the operators ship).
- naga does not fold constant overflow in function bodies, so `2147483647 + 1` wraps rather
  than failing; another WGSL compiler (Tint) would reject such a shader. wgpu always uses
  naga, so this is consistent today.
