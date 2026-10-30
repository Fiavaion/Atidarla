# M2 pre-work: geometry core (GEO wires, attribute buffers, PNT operators, PT-2)

**Status:** built ahead of M2's formal start at Mark's request (2026-10-18), for review. Nothing here relies on a PRD decision still marked Proposed; the open questions are at the end. Design: [`docs/design/geometry-attributes.md`](../../design/geometry-attributes.md) (deviations and numbers in its §9). Covers M2 AC1 (PT-2) and the framework part of AC5.

## What was built

| # | Item | Where |
|---|---|---|
| 1 | **GEO wires in the engine.** A `Geometry` value (points; prims as CSR topology; point, prim and detail attributes; groups as bits; a GPU header with the count and indirect arguments), a GEO results map, a `GeoOp` trait and `GeoCtx` beside `GpuOp`, GEO inputs for IMG operators (`CookCtx::geo_inputs`), caching and dirty propagation as for IMG, the **ownership hand-over** (a value moves to its only reader when nothing else can read it, so deformers write in place), simulation state on **GEO feedback** wires (including a node's own output), offline clearing of new buffers. `Scheduler::continuous` tells the engine which planned nodes cook again next frame. | `crates/atd-gpu/src/engine.rs`, `crates/atd-gpu/src/geo/mod.rs`, `crates/atd-core/src/cook.rs` |
| 1 | **Device limits** raised to what the adapter offers, up to 16 storage buffers per stage, 1 GiB bindings and buffers, 8192 textures (`atd_gpu::required_limits`); used by the app, headless devices and the render tests. | `crates/atd-gpu/src/lib.rs`, `crates/atd-app/src/main.rs`, `crates/atd-cli/src/render.rs` |
| 2 | **Attribute-buffer framework.** Layout rules and types (16-byte stride for 3-component types), a size-classed `BufferPool` (powers of two from 4 KiB, deferred release after submit, trimming like the texture pool), per-attribute residency (CPU, GPU or both; uploads and readbacks only on demand; readbacks never wait live and wait offline), compute building blocks: `Kernel` with a bind-group cache, multi-level `Scan`, `Compactor`, the generic `Gather`, `Deformer` (in place or copy on write, same WGSL), the shared WGSL library (hash, gradient noise with analytic derivatives, curl noise). | `crates/atd-core/src/geo.rs`, `crates/atd-gpu/src/geo/` |
| 3 | **Operators.** `pnt.points` (grid, random, line, circle; seed, count), `pnt.noise` (curl noise displacing `P` or accelerating `v`), `img.render_points` (instanced 2-px quads, vertex pulling, `Cd` or constant, add or over, orthographic), with schemas, budgets, generated docs pages, examples, GPU tests and offline classes. | `crates/atd-ops-pnt/`, `examples/points.atidarla`, `docs/operators/` |
| 4 | **PT-2 core.** `pnt.particles` (capacity, rate, initial burst, life and variance, emitter, velocity and spread, curl noise, attraction, gravity, drag, colour by age; compaction by prefix sum, no atomics; state from the `prev` feedback input or its own result); bench case `pt2.particles_2m`; spikes S1, S2 and S3 (deformer stack and CPU round trip). | `crates/atd-ops-pnt/src/particles.rs`, `crates/atd-cli/src/bench.rs`, `crates/atd-ops-pnt/tests/spikes.rs`, `examples/particles.atidarla` |
| 5 | Implementation notes in the design doc (§9) and this file. | |

Offline classes (`classify()`, `docs/render.md`): Pure: `img.render_points`, `pnt.points`, `pnt.noise`; Stateful: `pnt.particles`. No GEO operator works on a worker thread yet, so none waits; `Readback::receive`/`take_mapped` already wait offline (`CookMode::Offline`) for the CPU operators to come.

## Evidence (actually run, 2026-10-18, RTX 5070 Ti, Windows 11, DX12 unless noted)

Other agents were compiling and running GPU work on the same machine throughout, so every timing is noisy; the numbers below are from release builds after the final commits.

| Check | Result |
|---|---|
| `cargo test --workspace` | 321 passed, 0 failed, 5 ignored (the 4 spikes and 1 existing) |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` | clean |
| `cargo run -p atd-cli -- docs --check` | up to date |
| `cargo run -p atd-cli -- check --gpu examples/*.atidarla` | 13 of 13 ok (including the new `points`, `particles`) |
| `atidarla render` twice, frames 0-120, SHA-256 per frame | `particles`: 120/120 identical (120 distinct images); `points`: 120/120 identical (120 distinct) |
| `atidarla bench` (release), `pt2.particles_2m`: 2M particles, curl noise + attract + age, 2-px additive sprites at 1920×1080, 300 frames | DX12: p50 1.86 ms, p95 2.02 ms, max 2.37 ms; GPU p50 simulate 0.75 ms, simulate + draw 1.78 ms (draw ≈ 1.02 ms). Vulkan (`WGPU_BACKEND=vulkan`): p50 1.73 ms, p95 1.80 ms; simulate 0.64 ms, draw ≈ 1.03 ms. (Final re-run; an earlier run gave p50 1.85 ms DX12, 1.74 ms Vulkan.) No buffers, bind groups or textures created after warm-up. Target p95 ≤ 16.7 ms is judged on the RTX 4070 lab PC (not measured there). PT-1 in the same runs: DX12 p50 5.55 ms, Vulkan 4.78 ms. |
| S1 (spike): 2M static sprites at 1080p | GPU p50 1.01 ms, p95 1.88 ms (≤ 3.0); render-thread cook p50 0.07 ms (≤ 0.3); 0 creations after frame 10 |
| S2 (spike): 2M particles | simulate GPU p50 0.89 ms (≤ 4.0); with draw 1.90 ms (≤ 7.0); two offline runs of 300 frames, the second with 32 spinning threads: 300/300 frames identical, final `P v age life id` identical |
| S3 (spike): 30 `pnt.noise` deformers over 2M points | GPU p50 4.53 ms, p95 5.23 ms: **over the 3.0 ms fusion threshold** (see questions) |
| S3 (spike): 100k-point GPU → CPU worker → GPU, live at 60 fps | latency exactly 2 frames (≤ 2); render-thread cook p50 0.31 ms, max 0.97 ms vs 0.23 / 1.06 ms without it (no stall > 0.5 ms from the round trip); `Queue::write_buffer` from a worker thread works |
| Allocation test (`atd-ops-pnt/tests/alloc.rs`) | 0 buffers, 0 bind groups, 0 textures created in 100 steady frames; 34.3 heap allocations per node-cook (bound 45; IMG's is 25). Breakdown (`ATD_ALLOC_BREAKDOWN=1`): an empty frame 17; wgpu about 16-24 per compute or render pass at submit and 4 per `write_buffer`; GEO code about 10 per frame over six nodes. |
| Noise library | analytic gradient vs central differences ≤ 3.8e-4; curl divergence ≤ 4e-3 |

Commands: `cargo run --release -p atd-cli -- bench`; `cargo test -p atd-ops-pnt --release --test spikes -- --ignored --nocapture --test-threads=1`.

## APIs other areas build on

- **`atd_core::geo`** (no GPU): `AttrClass`, `Scalar`, `AttrType` (`stride`, `wgsl`), standard `names` (`P N Cd uv v age life id pscale orient group`), `header` (layout, WGSL struct, `bytes`), `buffer_bytes`, `WORKGROUP`, `wgsl_binding` (declarations for the Kernel DSL and `pnt.wgsl`), `valid_name`.
- **`atd_gpu::engine`**: `GeoOp` (`cook`, `after_submit`, `status`), `GeoCtx` (`input`, `take_input`, `can_take`, `alloc`, `cpu_header`, `buffers`, `images`, `sig_inputs`, `output`, `params`, `time`, `dt`, `frame`, `cook_again`), `OpLibrary::add_geo`, `Engine::geometry`, `Engine::buffer_pool`; `CookCtx` gained `geo_inputs` and `buffers`.
- **`atd_gpu::geo`**: `Geometry`, `Attribute` (`gpu`, `cpu`, `from_vec3/f32/vec4/u32`, `ensure_gpu`, `unique_gpu`, `shares`, `residency`), `Topology`/`PrimKind`, `DetailValue`, `BufferPool`/`GeoBuffer`; `kernels` (`Kernel`, `Bind`, `Work`, `pass`, `BindGroupCache`, `Uniforms`, `Packer`, `Scan`, `Compactor`, `Gather`, `Deformer`); `readback` (`Readback` with `receive` and `take_mapped`, `Mapped`, `read_blocking`); `wgsl` (`HASH`, `NOISE`).
- **`atd_gpu::timer::GpuTimer`** for benches; `atd_gpu::required_limits`.
- **Writing a GEO operator:** take the input (`take_input`), deform with a `Deformer` or write new attributes from `ctx.alloc`, set `*ctx.output`; never write an attribute you do not hold uniquely; record work into `ctx.encoder` (several dispatches in one `pass`); register with `add_geo`; classify in `classify()`.
- **MSH (CPU) operators:** record a `Readback` in `cook`, `start` it in `after_submit`, `take_mapped` on a later cook and give the `Mapped` buffers to the worker; the worker allocates from a cloned `BufferPool` and uploads with `queue.write_buffer`; send the `Attribute` back and take it with `params.mode().receive(&rx)` (offline it waits). The S3 spike's `RoundTrip` is a working template.

## Not done

- The static `attributes` list in `OperatorSchema` (design §3.1): adding a field breaks about 40 struct literals in crates other agents are editing; to do as one coordinated change.
- A `pnt.solver` / Solver Block node (any GEO feedback input carries state already), For-Each, the `pnt.wgsl` node, deformer fusion, perspective or 3D camera in Render Points, To Channels / attribute spreadsheet, `.atdgeo` bakes, the app viewer for GEO nodes (view them through Render Points), a live particle-count readout (the count stays on the GPU).
- Heap allocations per frame are bounded, not zero: recycling the per-generation `Arc` of attribute values would remove the GEO code's share.
- Measurements on the RTX 4070 (PT-2's target machine) and on a Mac (PT-5, Metal parity of vertex-stage storage reads).
- The design doc's header still says "no code exists yet"; I was asked to add only §9 to it.

## Questions for Mark

1. **PT-2 definition** (design Q1): accept "2M particles, curl noise + attract + age, 2-px additive sprites coloured from `Cd`, no sorting, 1920×1080" as `pt2.particles_2m`? It is what the bench measures now.
2. **Fusion** (design D9): S3 measured 4.5 ms for 30 curl-noise deformers over 2M points, over the 3.0 ms threshold, so by the design's rule fusion becomes an M2 task. About half is bandwidth that fusion removes; the rest is noise arithmetic it does not. Build fusion in M2, or accept unfused stacks at this size (100k points: about 0.2 ms)?
3. **Rule 2 for GEO**: accept "no GPU object creation after warm-up, heap allocations bounded per node-cook" (34 measured, bound 45, mostly wgpu per pass) as for IMG, with the GEO code's own share removed later?
4. **Hand-over through feedback wires** (an extension of D4): a solver may take its loop's previous value when nothing else reads it. Any objection?
5. **Schema `attributes` field**: schedule it as one change across all operator crates (after this wave merges)?

## Decision-log rows: ratified 2026-10-30 (Mark accepted; IDs assigned, now in PRD §0)

| ID | Decision | Rationale |
|---|---|---|
| A26 (was GEO-1) | GEO values are reference-counted snapshots; an operator writes in place only into attributes it holds uniquely; the engine hands a value to its only reader when nothing else can read it (normal input: the source cooks every frame and is not viewed; feedback input: the source cooks again later in the frame; a node's own output). | Copy-free deformer stacks and in-place simulation state without per-operator special cases (design D4, §3.2; S3: 29 of 30 deformers in place, no buffer creation). |
| A27 (was GEO-2) | Devices request the WebGPU defaults raised to what the adapter offers, up to 16 storage buffers per stage, 1 GiB storage bindings and buffers and 8192 textures; every core operator still works at the defaults. | Raises point-count ceilings (2M × 16 B fits the defaults; 8M+ needs more) without excluding the floor hardware (design D12, §7). |
| A28 (was GEO-3) | `pt2.particles_2m` is the PT-2 benchmark: 2M particles, curl noise + attract + age, 2-px additive sprites coloured from an attribute, no sorting, 1920×1080, frame time including GPU completion. | The PRD does not say how particles are drawn (design Q1); measured 1.86 ms p50 (2.02 ms p95) on the RTX 5070 Ti. |
| A29 (was GEO-4) | GEO kernels use no atomics that decide placement and no float atomics; randomness is a hash of (seed, emission number or ID, frame); compaction is a multi-level integer scan. | Offline renders stay byte-identical (S2: 300/300 frames with every core busy) and the kernels run on DX12, Metal and Vulkan alike (design §4.3). |
