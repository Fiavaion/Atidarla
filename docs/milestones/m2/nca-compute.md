# M2 pre-work: Neural CA, the Compute node (FR-K5), sink wording

Written 2026-10-18 on branch `m2/nca-compute` (from `claude/compassionate-franklin-c9opw6` at
`f30b841`), while Mark was away and had authorised running ahead of "one milestone at a time".
Every other CLAUDE.md rule applies. Nothing here edits `PRD.md`, `M1.md`, `M2.md` or `HANDOFF.md`;
the decision-log rows proposed below are for Mark to accept, amend or reject.

## What was built

| Item | Where | Reference |
|---|---|---|
| **`img.nca` Neural CA**, the fourth 2D solver of PRD §9 M2 | `crates/lsr-ops-img/src/nca.rs`, tests `tests/nca.rs` | `docs/nca.md` |
| **`img.compute` Compute**, FR-K5's raw WGSL node with `#bind`, tile size and iterations | `crates/lsr-ops-img/src/compute.rs`, `src/watch.rs`, tests `tests/compute.rs` | `docs/compute.md` |
| Generator wording: sinks are *requested* every frame and cook only when dirty | `crates/lsr-cli/src/docs.rs`, regenerated `docs/operators/` | commit `9d310d2` |
| Examples | `examples/neural-growth.lasair`, `examples/compute-shaders.lasair` | `examples/README.md` |

Both operators ship complete (rule 7): schema with help and cook budget, generated docs page,
runnable example cooked by `lasair check --gpu`, unit and GPU pixel tests, classification for
offline render (`classify()` in `crates/lsr-cli/src/render.rs`, table in `docs/render.md`), and
steady-state allocation checks (`tests/alloc.rs`). `img.wgsl` is unchanged.

### Neural CA (`img.nca`)

Per-cell state of 4 to 16 channels (colour, alpha, hidden) in two `f32` storage buffers
ping-ponged on the GPU; perception by fixed identity, Sobel x, Sobel y and Laplacian filters; a
one-hidden-layer ReLU MLP; stochastic update through an integer PCG hash of (cell, step, seed),
so it is deterministic on any GPU; alive masking (3×3 alpha max above a threshold, before and
after the step); an `erase` input for regeneration; seeds `centre`, `scatter`, `noise` or the
`seed` image; Reset as a pulse; Steps per frame; Cell Size; zero or wrapped edges; `rgba` or
`hidden` display over a background, square or smooth cells.

Weights: built-in rules `grow` and `spots`, or a JSON weights file in the project (format in
`docs/nca.md`, canonical writer, CRLF-safe parser, reload on save, worker-thread parse). **No
weights from elsewhere** (rule 4): the built-in rules and the example's `nca/coral.json` are
*hand-built* by `GrowRule` (alpha growth into neighbours, a piecewise-linear activator-inhibitor
Turing pattern in two hidden channels, colour relaxing towards them), tuned first with a NumPy
prototype and then written in Rust. A test checks the shipped file equals the generator's
output; an ignored test rewrites it.

Performance: each network shape gets its own update shader (channels, hidden units and weight
offsets as constants, perception and per-unit sums written out so they stay in registers, dead
neighbourhoods skip the network), built on a worker thread when the shape changes.

### Compute (`img.compute`)

A user `fn kernel(id: vec2u)` per output pixel; directives `#bind input|output|state|param|buffer`
(full table in `docs/compute.md`); Tile menu `8x8` … `256x1`; Iterations 1 to 256 with one
uniform block per dispatch (dynamic offsets) and `iteration()`; `state` pairs swap per iteration
and persist across frames (or load from an input each cook with `from=inN`); buffers persist and
can be sized `pixels`; Reset clears both; inline code or a watched file, compile on a worker
thread, errors `line L, col C: …`, the last good program keeps running, and a new program with
the same images and buffers keeps their contents (live coding keeps the state). The shown image
is copied into the engine output through a small render pass, because engine textures have no
storage usage and the shown image may be any of the four storage formats.

**Storage formats:** `rgba8unorm`, `rgba16float`, `rgba32float`, `r32float`. In wgpu 30's
`TextureFormat::guaranteed_format_features(Features::empty())` each has `STORAGE_BINDING` with
`STORAGE_WRITE_ONLY`, plus `TEXTURE_BINDING` and `RENDER_ATTACHMENT` (used to clear on Reset):
the WebGPU core set, which wgpu provides on DX12, Metal and Vulkan without optional features. Not
offered: `bgra8unorm` (needs `BGRA8UNORM_STORAGE`), the 16-bit norm formats and read-write access
to anything but `r32float` (adapter-specific). The 32-bit float formats are unfilterable; the
operator binds them as such and the docs say to use `textureLoad`. Limits used, all within
`wgpu::Limits::downlevel_defaults()`: 4 writable images (`max_storage_textures_per_shader_stage`),
4 buffers (`max_storage_buffers_per_shader_stage` at the downlevel floor), 160-byte uniform
blocks at a 256-byte stride (`min_uniform_buffer_offset_alignment`), workgroups of at most 256
invocations and 256 wide, buffers up to the device's `max_storage_buffer_binding_size`.

## Evidence (run on the RTX 5070 Ti PC, Windows 11, DX12, while four other agents were
compiling and running GPU tests)

- `cargo test -p lsr-ops-img --lib`: 17 passed (new: 4 Compute, 2 Neural CA).
- `LSR_REQUIRE_GPU=1 cargo test -p lsr-ops-img --test compute`: 5 passed (blur vs CPU within
  1/255 at every tile size; 8 diffusion iterations vs CPU within 1/255; state and buffer persist,
  5 iterations add exactly 10/255 per cook, Reset clears, an edit keeps state; syntax, directive
  and validation errors with line/col while the last image stays; file reload; all four formats).
- `LSR_REQUIRE_GPU=1 cargo test -p lsr-ops-img --test nca`: 4 passed, 2 ignored (16 steps from a
  noise seed equal a CPU reference **with no value off by more than 1/255** over 4 frames; growth
  from the centre and regrowth into an erased hole; file weights byte-identical to the built-in
  rule frame by frame, CRLF file, broken file reported and recovered; the shipped weights file
  equals the generator).
- Neural CA frame time (`--ignored frame_time`, release): 640×360 grid, 8 steps, every cell
  alive: **1.18 ms per frame** (CPU cook 38.7 µs); 14.77 ms with the first, generic shader.
- `tests/alloc.rs` new `compute_and_nca_steady_state_allocations`, after both compiled: a
  one-pass operator 14.8 allocations per frame, Compute (5 dispatches and the copy) 56.8, Neural
  CA (18 dispatches and the copy) 118.8. Varying Iterations 1/2/8/16 gave 45.1/49.0/72.0/99.0, so
  about 3.6 per dispatch, all inside wgpu's pass recording; the operators' own paths hit their
  bind-group caches (no bind groups or views per frame). The existing per-operator test caught
  both while still compiling (13.8), which is why the dedicated test waits.
- `lasair check --gpu examples/neural-growth.lasair examples/compute-shaders.lasair`: ok
  (each reports 4.0 s because a watched file keeps the node pending through the 3 s settle, as
  Analyze does in signal-scope).
- `lasair render` of both examples (release): neural-growth 900 frames at 1280×720 and each
  node alone for 600 frames; compute-shaders 90 frames; frames inspected by eye (labyrinth
  organism with the eraser's regrowing cut over dimmed coral; blur, glow and trails).
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`,
  `lasair docs --check`: pass. `lasair check --gpu examples/*.lasair`: 13 ok.
- Full `LSR_REQUIRE_GPU=1 cargo test --workspace` at `e65a249`: **311 passed, 0 failed, 3
  ignored** (72 test binaries; the new ignored ones are `frame_time` and
  `write_example_weights`).

## Open questions for Mark

1. **FR-K5 as a separate operator.** `img.compute` sits beside `img.wgsl` instead of extending
   it (the fragment form is simpler for one-colour-per-pixel shaders and its projects keep
   working). Yes, or merge them later?
2. **The `#bind` syntax** (`input`, `output`, `state` with `from=`, `param`, `buffer` with
   `pixels`) is designed here, inspired by but not copied from Houdini's OpenCL node. Tile and
   iterations are parameters, not directives (the geometry design note sketches `#tile` and
   `#iterations` directives; directives could set the defaults later). Accept?
3. **Entry point.** Only `fn kernel(id: vec2u)` with a generated bounds check; no user
   `@compute` entry with workgroup barriers yet. Wanted for M2?
4. **Named parameters in the UI.** Schemas are per type, so the panel still says P1…P8; names
   show in the node's status line. Per-node parameter labels would need a schema change (Packs'
   promoted parameters may want the same).
5. **On a compile or weights error** the node shows the error and keeps its last image (the
   engine drops a failed cook's output, as for WGSL), so a stateful node pauses rather than
   running on with the old program. Running on would need an engine change (an "ok with warning"
   result).
6. **Reset is a pulse** on both new operators (reset on any change of its value, so a click, or a
   bound trigger's edges), where Ripple, Reaction-Diffusion and Fluid use a "while on" toggle.
   Align them one way?
7. **Neural CA semantics.** Perception adds a Laplacian to the paper's identity and Sobel pair
   (the hand-built rules need it); the "after" alive mask is computed on the pre-masked update (a
   two-pass simplification). Trained models would need training-time code to match; fine?
8. **Trained models.** Only hand-built weights exist. Whether to support trained NCA models
   through the model manager (with licence metadata, rule 4), and in which format (this JSON, or
   ONNX via the planned ONNX node), is open.

### Decision-log rows: ratified 2026-10-30 (Mark accepted; IDs assigned, now in PRD §0)

| ID | Decision | Rationale |
|---|---|---|
| A42 (was A26) | FR-K5 for images is a separate `img.compute` operator with `#bind` directives (`input`, `output`, `state`, `param`, `buffer`), Tile and Iterations parameters, a user `fn kernel(id: vec2u)`, and storage formats limited to the four wgpu guarantees on every backend. `img.wgsl` stays the fragment form. | Rule 6 parity with no optional features; keeps WGSL projects working; one directive grammar to extend to geometry. |
| A43 (was A27) | The Neural CA ships only hand-built weights, generated by code in the repository (`GrowRule`) with a test that the shipped file matches; any other weights come from a project file (JSON with licence and source fields) or later the model manager. | Rule 4. |

## How `#bind` should extend to geometry (for the geometry area's FR-K5 follow-up)

The parser in `compute.rs` (`parse`, `Plan`, `Decl`, `generate`, line/col errors, reserved
names, the downlevel limits test) is written to be extended rather than copied:

- **New kinds for attributes**, following `docs/design/geometry-attributes.md` §5 and D10:
  `#bind point P vec3f read_write`, `#bind point v vec3f read`, `#bind point age f32 create
  default=0`, and `prim`, `vertex`, `detail` classes. The resolved plan comes from the upstream
  attribute schema at plan time; the binding generator in `lsr-gpu`/`lsr-core` (D10) writes the
  declarations, so the image `#bind` kinds and the attribute kinds share one `Decl` list and one
  binding order (declaration order, deterministic, rule 8).
- **Kernel signature by class**: `fn point(i: u32)` runs once per point (1D dispatch; Tile becomes
  the 1D workgroup size, 64/128/256), as `fn kernel(id: vec2u)` runs once per pixel. A node binds
  either images or one geometry class as its iteration domain.
- **Iterations**: per-dispatch uniform blocks and `iteration()` carry over unchanged. Attributes
  marked `read_write` are written in place (gather-only element writes, §5 point 3, keep that
  race-free); an explicit `state`-style pair is only needed for neighbour reads (`nearpoints()`
  style), and should reuse the `state`/`NAME_out` naming.
- **Parameters and buffers** (`#bind param`, `#bind buffer`) carry over as they are, including
  `atomic_u32` buffers for counts and histograms; a `points` count (like `pixels`) would size a
  buffer to the point count.
- **Errors and reload**: the same worker-thread compile, line/col mapping (directive lines
  become blank lines), and "same resources keep their contents" rule give FR-K4-style state
  preservation for free when attribute names and types are unchanged.

## Not done

- A "Generated WGSL" view in the app, and a user-written `@compute` entry point (above).
- Trained Neural CA models (above); a `#bind` for Neural CA (it has its own fixed shaders).
- `.gitattributes` has no rule for `.json`, so `coral.json` may check out with CRLF on Windows;
  the parser and the golden test accept both.

## What other areas should know

- `lsr-ops-img` now depends on `naga` and `serde_json` (both already workspace dependencies).
- Shared files touched minimally: `crates/lsr-ops-img/src/lib.rs` (two `mod` lines, one schema,
  one factory; the registry-count test is now `+ 8`), `stateful.rs` (one `STATEFUL` row),
  `crates/lsr-cli/src/render.rs` (two `classify()` arms), `docs/render.md` (two table cells),
  `examples/README.md` (two rows and a note on generated weights), regenerated `docs/operators/`.
- `crates/lsr-ops-img/src/watch.rs` is a generic file watcher (build each new version on its own
  thread) that Packs or Python ops could reuse; WGSL keeps its own copy, unchanged.
- Test binaries `tests/compute.rs` and `tests/nca.rs` serialise their GPU tests on a mutex (WARP
  and parallel compute), as `tests/analyze.rs` does; the new allocation test shares
  `tests/alloc.rs`'s existing mutex.
