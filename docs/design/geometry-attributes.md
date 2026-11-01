# Geometry attributes and GPU/CPU residency (M2 design)

**Status:** draft for review, 2026-10-18. Design only; no code exists yet.
**Covers:** the "attribute-buffer framework with GPU/CPU residency" of PRD §9 M2, on which PNT (≥30 ops), MSH (≥18) and MAT (≥6) are built, plus Render IMG.
**Sources:** PRD §5.2–5.4, §6, §8 (PT-2, PT-5, PT-10, PT-12), FR-K1–K5, FR-H1; RESEARCH §2.3, §5.3, §5.4; `docs/render.md`; `docs/spikes/msh-licences.md`.
**Reading guide:** the table below is the whole design in one page. Sections 1–7 give options and reasons. Section 8 separates what I decided from what only Mark can decide.

## Decisions at a glance

| # | Topic | Recommendation | Decided by |
|---|---|---|---|
| D1 | Data model | Points and primitives, with point, prim and detail attributes; groups as bits; no vertex class in M2 (seams split points) | Designer; vertex deferral needs Mark's confirmation (Q2) |
| D2 | GPU layout | Structure of arrays (SoA): one storage buffer per attribute; 3-component types at a 16-byte stride on both GPU and CPU | Designer |
| D3 | Residency | Tracked per attribute (CPU, GPU or both); moved only when an operator asks for the other side; one wire badge summarises it | Designer |
| D4 | Ownership | A `GEO` value is immutable once published and reference-counted; an operator may write in place only into a buffer it holds uniquely; the engine hands over sole ownership when the source's cache is useless | Designer |
| D5 | Pools | A size-classed buffer pool beside the texture pool, with the same trim policy; particle capacity is a parameter, so a running simulation never grows a buffer | Designer |
| D6 | Simulation state | Lives on the wire (a GEO feedback loop through a Solver node), not inside operators | Designer |
| D7 | Spawn and death | Fixed capacity, compacted every frame by an integer prefix sum while the state is copied (stable, deterministic order); no atomics decide where a particle goes | Designer |
| D8 | Draw path | Instanced quads with vertex pulling from storage buffers, sized by an indirect draw | Designer |
| D9 | Deformer stacks | Hand the buffer over in place first; fuse runs of per-point operators into one dispatch only if spike S3 shows it is needed (it did: built 2026-10-30, §9) | Designer, after S3 |
| D10 | Kernel and WGSL contract | The schema, layout and binding generator live in `lsr-core`/`lsr-gpu`. A Kernel (A12) and a `pnt.wgsl` node with `#bind` (FR-K5) are both clients | Designer; A12 itself is Mark's |
| D11 | Files | Networks stay text. Generated geometry is never saved. Bakes are rebuildable caches in `.cache/`; assets leave through glTF/PLY | Designer; bake policy needs Mark (Q3) |
| D12 | Limits | Rely only on wgpu `Limits::default()` (the WebGPU defaults), which is what the app and CLI request today; ask for more where the adapter offers it | Designer |

## 0. What exists today (what the design must fit)

- **Wire type.** `WireType::Geo` exists (`crates/lsr-core/src/registry.rs`). Wires connect only between equal types (`Network::validate_wire`, `crates/lsr-core/src/graph.rs`), so PNT↔MSH wiring with no conversion is already enforced by type. Sockets may be `feedback` (they read the previous frame's result, so a loop is allowed).
- **Scheduler.** `crates/lsr-core/src/cook.rs` plans pull-cooks in dependency order: a node cooks if it is dirty, never cooked, time-dependent, fed by a feedback input, or downstream of a node cooking this frame. The plan allocates nothing once warm (`tests/plan_alloc.rs`). It has no notion of a node's data type.
- **Engine.** `crates/lsr-gpu/src/engine.rs` keeps per-node results as `HashMap<NodeId, wgpu::Texture>` (IMG) and `HashMap<NodeId, Channels>` (SIG). It sends every node whose first output is not SIG down the IMG path, so a GEO node today would be cooked as an IMG node. It records all GPU work into one encoder per frame and releases replaced textures to the pool after submit. `CookMode::{Live, Offline}` and `CookMode::receive` implement the offline rules in `docs/render.md` (A25).
- **Pools.** Only a texture pool exists (`crates/lsr-gpu/src/pool.rs`: bucketed by key, a free texture is dropped after 300 frames, free memory is capped at 512 MiB). There is no buffer pool. `FullscreenPass` (`crates/lsr-gpu/src/fullscreen.rs`) caches bind groups by input identity (an LRU), so ping-ponged pooled resources hit the cache. GEO dispatches can reuse that pattern.
- **Stateful precedent.** The IMG solvers keep `PingPong` textures inside the operator (`crates/lsr-ops-img/src/stateful.rs`, `fluid.rs`) and copy the result to a pooled output.
- **Stubs.** `lsr-ops-pnt`, `lsr-ops-msh` and `lsr-ops-mat` register nothing and depend only on `lsr-core`. `lsr-kernel` has no dependencies. `lasair bake` prints "not yet (M2)" (`crates/lsr-cli/src/main.rs`).
- **Device limits.** `lsr-app/src/main.rs` asks for `wgpu::Limits::default()` (with `max_texture_dimension_2d` raised to 8192). `GpuContext::headless` and the render tests ask for the default too. So today every device has 8 storage buffers per stage, a 128 MiB storage binding and a 256 MiB buffer, whatever the hardware can do.
- **FR-K5 gap (found while reading).** `img.wgsl` (`crates/lsr-ops-img/src/wgsl.rs`) is a full-screen fragment shader with fixed inputs `in0`/`in1` and parameters `p1()`…`p4()`. It has no `#bind` declarations, no tile size and no iterations, which FR-K5 asks for. `M2.md` lists FR-K5 as done in M1; for geometry it is not (see §5).

## 1. Data model: what a `GEO` value is

PRD §5.4 says: "attribute buffers: point/prim/detail, groups, topology". RESEARCH §2.3 says "vertex attributes deferred". The M2 operator list (§6, M2 in §9) needs points with attributes (every PNT op), polygon and polyline topology (Box…Torus, Extrude, Boolean, Subdivide, PolyReduce, Sweep, Skin, Triangulate), UVs (UV Project, glTF out) and normals.

```
Geometry
├─ points: count (or a GPU-side count, §4) ── point attributes: P (always), others by name
├─ prims:  count, kind per prim (polygon | polyline)
│          topology as CSR: prim_start[prims+1], prim_points[corners]  (corner → point index)
│          prim attributes by name
├─ detail: small named values (emitter ID counter, seeds, bounds), CPU-first
└─ groups: per class, up to 32 names → one bit each in a u32 "group mask" attribute
```

**Attribute type** = scalar kind (`f32`, `i32`, `u32`) × components (1–4), plus an optional *semantic* (position, normal, colour, uv, quaternion, id) that the viewer, the renderer and the exporters use. Standard names are `P`, `N`, `Cd`, `uv`, `v`, `age`, `life`, `id`, `pscale`, `orient` (RESEARCH §5.4 uses the same `@P`, `@Cd`, `@v`, `@id`).

| Option | For | Against |
|---|---|---|
| A. Points + prims + detail, no vertex class (**recommended**) | Matches §5.4. One element per GPU vertex, so drawing needs no indirection. glTF and PLY are point-indexed too | UV seams and hard edges must split points. Recombining needs Fuse |
| B. Full four-class model (point/vertex/prim/detail) | Seams without splitting; closest to Houdini | Every GPU consumer must resolve vertex→point; doubles the binding and layout cases; RESEARCH §2.3 already deferred it |
| C. Points only, meshes as a separate type | Simplest GPU side | Breaks "one GEO wire, two families" (A10, ratified) |

**Groups as bits.** One `u32` mask attribute per class holds up to 32 groups. That is one storage binding however many groups exist, which matters under the 8-binding floor (§7).

**Deliberately left out of M2:** vertex attributes; string and array attributes (a `name` prim attribute for Fracture pieces waits for this); `f16`/8-bit storage types (`SHADER_F16` is not universal); volumes and voxels (Iso Surface builds its field internally from points and radii); NURBS, Bézier and spline primitives (Line/Curve are polylines); instancing as a primitive kind (Copy to Points/Instance writes per-instance point attributes that Render IMG reads); more than 32 groups per class. Each can be added without changing the layout rules in §2.

## 2. Storage and residency

### 2.1 GPU layout

| Option | For | Against |
|---|---|---|
| **SoA, one buffer per attribute (recommended)** | Per-attribute sharing and copy-on-write (a deformer that writes `P` shares every other buffer); coalesced loads; matches RESEARCH §5.3; the layout the Kernel DSL binds `@attr` to | Bindings grow with the attributes a pass touches (8 per stage minimum, §7) |
| AoS (one struct per point) | One binding | The layout changes whenever an attribute is added; every pass moves every attribute; no sharing |
| SoA in one arena buffer per geometry | One binding for any attribute set (accessors use `bitcast`) | Copy-on-write granularity is the whole arena; awkward accessor code for every op and for kernels |

Layout rules (fixed now, so the CPU, kernels and files agree): little-endian; `vec3` types use a 16-byte stride (WGSL `array<vec3<f32>>` has stride 16 anyway, and a CPU `Vec3A`-style type matches, so upload and readback are plain copies); offsets and sizes rounded to 256 bytes (`min_storage_buffer_offset_alignment`). Buffers use `STORAGE | COPY_SRC | COPY_DST`; triangle index buffers add `INDEX`.

Operators that move every attribute (Delete, Sort, Merge, compaction) use one generic gather kernel per element size (4, 8 or 16 bytes). Each call binds a map, a source, a destination and a header, which is 4 bindings whatever the attribute count. They dispatch it once per attribute, or use `copy_buffer_to_buffer` for contiguous ranges (Merge).

### 2.2 Ownership and residency

```
           upload (on change, staging copy)            readback (async copy + map)
  CPU only ───────────────────────────────►  both  ◄───────────────────────────── GPU only
  (MSH cook output)                     (in sync, same version)               (PNT op output)
```

- **Per attribute, not per geometry.** Each attribute holds an optional CPU copy and an optional GPU buffer, plus a version number. When a PNT deformer writes `P` after an MSH op, `N` and `uv` stay "both" and only `P` becomes GPU-only. The next MSH op then reads back `P` alone. The wire badge (AC5) shows the worst case and the bytes moved by the last cook.
- **A published value never changes.** A `GEO` value is a reference-counted, read-only snapshot. Writing creates a new attribute, from a pooled buffer, and shares the rest. An operator may write in place only into a buffer it holds uniquely (`Arc::get_mut` semantics). This one rule covers caching, ping-pong and in-place updates (§3.2).
- **Release is deferred.** A dropped GPU buffer goes onto a release list that the engine drains after `queue.submit`, as replaced textures already do (`released` in `engine.rs`), because a submitted frame may still read it.
- **Transfers happen only at a family boundary**: a CPU operator (MSH, Pixel Out, To Channels, the attribute spreadsheet) reading a GPU-only attribute, or a GPU operator or Render IMG reading a CPU-only one. Nothing moves "just in case".

### 2.3 Rule 1 (never block the render thread): MSH cooks

1. The render thread plans the MSH node. If any input attribute it reads is GPU-only, it records a copy into a pooled staging buffer *in this frame's encoder*. That captures this frame's data even if the source is overwritten later. After submit it starts `map_async`.
2. When the map completes, the input (CPU copies) goes to an MSH worker thread with the job. Manifold, meshoptimizer and xatlas run there (`docs/spikes/msh-licences.md`).
3. The worker uploads the result. I infer that `wgpu::Queue::write_buffer` can be called from a worker (wgpu 30's `Queue` is `Send + Sync` outside wasm); spike S3 must confirm this. The worker writes into buffers it takes from a thread-safe pool handle, so the render thread only swaps a pointer.
4. **Live:** the node outputs its last finished result (empty before the first) and sets `cook_again`, as `img.wgsl` does while a shader compiles. A GPU-resident input therefore makes an MSH result at least one frame late, and a cook longer than a frame keeps the previous mesh. That is the "over its frame budget" warning of PRD §6.
5. **Offline** (`CookMode::Offline`, `docs/render.md` rules 2–3): the cook waits. It calls `device.poll(wait)` for the readback, then `CookMode::receive` for the worker's result, so frame *i* uses the inputs of frame *i*. Classify every MSH op as "Waits" in `classify()` (`crates/lsr-cli/src/render.rs`). **Library determinism is not known:** Manifold's TBB `parallel` feature may reorder floating-point work. The task that adds Boolean must check it: run the same Boolean twice, with and without `parallel`, and compare the output. If two runs differ, offline MSH cooks run the library single-threaded.

### 2.4 Rule 2 (no allocation after warm-up): pools and growth

- **Buffer pool** (new, in `lsr-gpu`). Keyed by (size class, usage). Size classes are powers of two from 4 KiB, so a dragged Scatter count reuses a buffer instead of creating one per value. Trimming follows `TexturePool`: keep a free buffer for 300 frames and cap free memory, so a slider sweep cannot pin VRAM. Staging buffers for readback and upload are pooled the same way. Bind groups are cached by buffer identity, as `FullscreenPass` does.
- **Particle capacity is a parameter** (`max_particles`, default 1M, up to the device limit). Buffers are allocated at capacity when the parameter changes, which is an edit and not a frame-critical path. A running simulation never grows. If emission would overflow, it stops at capacity and the node shows a status line (`GpuOp::status`, A25).
- **Generators** (Point Shape, Scatter, primitives) allocate only when their count changes.
- **CPU side.** MSH workers may allocate (they are not frame-critical). The render-thread side of a GEO cook must pass an allocation-counting test like `tests/plan_alloc.rs`.
- **Ceiling under today's limits.** One binding is at most 128 MiB, which is 8,388,608 elements of a 16-byte type. With raised limits (§7) the ceiling is the adapter's.

## 3. Operator contract and the planner

### 3.1 What an operator declares

Two levels, because attribute names often come from parameters (Attribute Create's `name`):

| Level | Where | Contents | Used by |
|---|---|---|---|
| Static | `OperatorSchema`: a new optional `attributes` list, serialised like the rest of the schema | For each entry: class, name (literal or `$param`), type, access (`read`, `write`, `read_write`, `create`), whether it is required, and the default when missing | Docs pages, the MCP `get_schema` tool, the Palette, connection hints |
| Runtime | A new GEO operator trait in `lsr-gpu`, beside `GpuOp`/`SigOp` | `attributes(params, input schema) → plan`: the resolved list. `cook(ctx)` then receives exactly those bindings | The engine (bindings, transfers, ownership hand-over), Kernel/WGSL codegen |

The engine needs a third branch next to IMG and SIG in `Engine::cook`, chosen when the output wire is `Geo` (today such a node would be cooked as IMG). It also needs a `geo` results map. A missing required attribute is a node error that names it. A missing optional one gets its declared default, as a constant rather than an allocated buffer.

### 3.2 Planner behaviour on GEO wires

- **Dirty propagation and caching** need no change: GEO results are cached per node exactly like textures, and `cook.rs` already decides what cooks.
- **Ownership hand-over** (new, a small addition to the plan output). Before node *B* cooks, the engine gives *B* sole ownership of input *A*'s value when *A* cooked this frame, *B* is *A*'s only consumer, *A* is not requested (viewed or exported), and *A* will cook again next frame anyway (it is in the plan because it is time-dependent or downstream of something time-dependent, not because of a one-off edit). Then *B* writes `P` in place. Otherwise *B* copies on write. To do this the scheduler should report *why* each node is in the plan (a bitset, reused, so nothing allocates).
- **Simulation (Solver Block).** The state is the `GEO` on a feedback wire. A `pnt.solver` node has an `init` input and a `prev` feedback input. On frame 0, on Reset, or when `prev` has no result it passes `init`; otherwise it passes `prev`. This uses the feedback semantics the scheduler already has (test `feedback_loops_cook_every_frame_without_cycling`). The ping-pong is not special code: the pool and the uniqueness rule produce two alternating generations. When FR-P4 nested networks land, Solver Block becomes a container whose ports are this pair; the data contract does not change. Offline, it is "Stateful" in `docs/render.md`: state starts with the render.
- **For-Each.** M2 For-Each runs a body of per-element PNT ops once over all elements, with the piece or iteration index as an attribute (RESEARCH §2.3: "For-Each over points/instances compiled to one dispatch"). A fixed iteration count unrolls into N dispatches per op. The uniqueness rule keeps that allocation-free. Each iteration needs its own parameters, through a dynamic uniform offset, because `FullscreenPass`-style "one uniform buffer per pass" would make every iteration use the last values. Running an MSH body per piece is left out of M2.
- **30-op deformer stack.** At 2M points each per-point op reads and writes `P`: 2M × 32 B = 64 MB a pass. Thirty passes move 1.92 GB a frame, about 3.8 ms at the RTX 4070's 504 GB/s peak. The hand-over removes the allocation and the copies but not that traffic. Fusion removes it: deformers ship their WGSL as a function with a fixed signature over their declared attributes, and the engine joins a straight run of fusable ops into one pipeline (64 MB, about 0.13 ms), built on a worker thread when the graph changes. Fusion needs no Kernel DSL, so it does not depend on A12. **Recommendation:** build the hand-over in M2 and decide fusion from spike S3. At 100k points the unfused stack is about 0.2 ms and fusion does not matter. **Built 2026-10-30** (S3 was over its threshold; Mark ratified fusion for M2): see §9, "Deformer fusion".

## 4. Particle core for PT-2

PT-2: 2M particles, curl noise + attract + age, rendered at 1080p60 on an RTX 4070. The PRD does not say how particles are drawn. I propose 2-pixel additive sprites coloured from an attribute, with no sorting, as the benchmark definition (Q1).

### 4.1 Memory budget per particle

| Attribute | Type | Bytes |
|---|---|---|
| `P` | vec3f (16-byte stride) | 16 |
| `v` | vec3f | 16 |
| `age`, `life` | f32, f32 | 8 |
| `id` | u32 | 4 |
| **State, one generation** | | **44** |
| Optional `Cd` (vec4f) + `pscale` (f32) | | +20 |
| Scratch: alive flag + exclusive scan (u32 each) | | 8 |

Two generations live at once (the cached `prev` and the one being written): (2 × 44) + 8 = 96 B, so **192 MB at 2M**; with `Cd` and `pscale` 136 B, so **272 MB**. That is about 2% of 12 GB. PT-5 (500k on Apple M-series) needs 48–68 MB.

`id` is a wrapping u32 counter kept in detail. At 1M emissions a second it repeats after about 71 minutes; IDs are unique among live particles as long as capacity is below 2³². A 64-bit ID needs `SHADER_INT64`, which DX12 has only with DXC and Metal only with MSL 2.3+, so I did not choose it.

### 4.2 Dispatch structure (one frame)

```
 ┌───────────── prev (feedback: last frame's end) ─────────────┐
 ▼                                                              │
Solver ─► Particles ──────────────────────────► Curl Noise ─► Attract ─► end ─► Render IMG
          1 update: age += dt, P += v·dt,        v += curl·dt   v += …           draw_indirect,
            flag = age < life                    (in place via the hand-over)      quads, vertex pulling
          2 exclusive scan of flags (3–5 dispatches)
          3 gather survivors into the new generation, then append
            emissions at [survivors, survivors+emit) and write the
            count and the indirect args
```

- Every later pass uses `dispatch_workgroups_indirect` from the count written in step 3. The count stays on the GPU. The CPU sees it one frame late, through an async readback for the UI (exact offline).
- 2M ÷ 256 = 7,813 workgroups, within the 65,535 per dimension limit (one-dimensional dispatch works up to 16.7M elements).
- Estimated traffic, not measured: update 88 MB, scan about 24 MB, gather 192 MB, curl 96 MB, attract 96 MB, draw 32 MB: about 530 MB a frame, so about 1–1.5 ms on a 4070. The risk is raster cost (2M small quads), not compute.

### 4.3 Spawn and death

| Option | Deterministic (FR-H1)? | Cost | Verdict |
|---|---|---|---|
| Fixed pool with alive flags, dead slots skipped | Yes | Every pass runs at capacity; every consumer, including MSH and To Channels, must honour the flag | Fallback |
| Free/alive lists through `atomicAdd` | **No**: which slot a particle gets depends on thread timing, so draw order and IDs differ between runs | Cheapest | Rejected |
| Ring buffer (emit at head, oldest overwritten) | Yes | Correct only when every particle has the same life | Rejected for the general case |
| **Compaction by prefix sum during the generation copy (recommended)** | Yes: survivors keep their relative order and new particles append in emission order | One scan and one gather a frame. The gather is the ping-pong copy we pay for anyway | Recommended |

Determinism rule for every GEO kernel: integer atomics are allowed only where the final total is all that is used (the result does not depend on order). Atomics whose returned value decides placement, and float atomics (`SHADER_FLOAT32_ATOMIC` is missing on DX12 anyway), are not allowed. Random numbers are a hash of (seed, id or emission index, frame), with no RNG state. Scans use a portable multi-level reduce-then-scan, not single-pass decoupled look-back, because WebGPU and Metal do not guarantee forward progress.

### 4.4 Draw path

WebGPU point primitives are always one pixel (WGSL has no point size), so sprites are **instanced quads**: 4 vertices as a triangle strip and one instance per particle. The vertex shader pulls `P`, `Cd` and `pscale` from storage buffers by `instance_index`. `draw_indirect` takes its instance count from the count buffer. Meshes use the same vertex pulling with an index buffer, so Render IMG has one binding model for both families. If S1 finds the raster cost too high, a compute rasteriser for 1-pixel points (integer `atomicAdd` into a buffer, which is deterministic) is the fallback.

### 4.5 First M2 tasks: three spikes, in order, each ≤1 day

The numbers are on this RTX 5070 Ti (DX12). Its 896 GB/s is about 1.8× the 4070's 504 GB/s, so the budgets below leave room for that; PT-2 itself must be confirmed on the 4070 lab PC. Each spike writes `docs/spikes/<name>.md` with its numbers.

| # | Spike | Success numbers |
|---|---|---|
| S1 | **Buffer pool + GEO result path + 2M-sprite draw.** A static 2M-point grid drawn as 2-px additive quads at 1080p through vertex pulling and `draw_indirect`, on DX12 and on a Mac (500k) | Draw ≤ 3.0 ms GPU (timestamp queries) at 2M on the 5070 Ti; ≤ 4 ms at 500k on M-series; zero buffer or bind-group creations after frame 10 (pool counter); render-thread cook ≤ 0.3 ms; vertex-stage storage reads work on DX12 and Metal within 8 bindings |
| S2 | **Simulation step.** Solver feedback loop, update, scan, gather and emit, curl noise, attract, at 2M | Compute ≤ 4.0 ms and S1 + S2 ≤ 7.0 ms GPU on the 5070 Ti (the 4070 then has about 12.5 ms of its 16.7 ms); after 300 offline frames, the SHA-256 of `P` is identical across two runs and with every core busy; no allocation on the render thread after warm-up |
| S3 | **Deformer stack and family boundary.** 30 per-point ops over 2M points, unfused (with the hand-over) and fused; then a 100k-point PNT → CPU → GPU round trip | Fusion becomes an M2 task only if unfused > 3.0 ms GPU on the 5070 Ti; the round trip is ≤ 2 frames late live, with no render-thread stall > 0.5 ms; `Queue::write_buffer` from a worker confirmed |

## 5. Interface to the Kernel DSL (A12, Proposed)

The framework guarantees these things. Neither a Kernel (A12) nor a raw WGSL node needs anything more:

1. **A queryable schema before cooking:** the names, class and type of every input attribute are known when the node is planned. FR-K1 bindings and FR-K4 reload keys ("keyed by attribute names and types") come from that.
2. **Fixed layout rules** (§2.1) and a **binding generator** in `lsr-gpu` that writes the WGSL declarations for a resolved attribute plan. Group 0 belongs to the engine (a header with the count and capacity, a parameter uniform, detail). Group 1 holds the attributes in declaration order, each `read` or `read_write`. The pure-data part (types, layout, names) sits in `lsr-core`, so `lsr-kernel` can depend on it without depending on wgpu.
3. **Gather-only element writes:** invocation *i* writes only element *i*. That makes per-element kernels race-free and deterministic without atomics.
4. **State on the wire** (§3.2): editing a kernel recompiles its pipeline on a worker thread, and the next frame continues from the carried `GEO`. That is FR-K4/AC3 at no extra cost, provided a parameter edit keeps the engine's cached results, which it does today (`after_change` drops results only for removed nodes).
5. **Shared WGSL library:** the node-built ops (curl noise, attract) and the kernel helpers (FR-K2) call the same WGSL functions in the `lsr-gpu` library. Without that, AC2's "identical output" cannot hold.

**If A12 is rejected,** a `pnt.wgsl` compute node gives the same reach in M2 and completes FR-K5 for geometry:

```
#bind point P   vec3f read_write
#bind point v   vec3f read
#bind point age f32   create default=0
#tile 256
#iterations 1
fn point(i: u32) { P[i] += v[i] * dt(); age[i] += dt(); }
```

The engine parses the `#bind` lines into the runtime attribute plan (§3.1), generates the declarations, compiles on a worker thread as `img.wgsl` already does, and keeps the last good pipeline running.

**Risk for AC2:** a fused 10-line kernel and a multi-node chain are different shaders. A compiler may contract `a*b+c` into an FMA in one and not the other, or optimise across what were pass boundaries in the chain. "Byte-identical" may then fail by an ulp even when both are correct. See Q4.

## 6. Serialisation (rule 8)

- **Project files hold no geometry.** Networks stay `networks/<id>.toml` (`crates/lsr-format/src/network.rs`: nodes sorted by stable ID, inputs and params by name, only non-default params). Attribute names, types and groups are ordinary parameters (string, menu, vec), so the format needs no change. Kernel sources go in `kernels/*.atk` and WGSL in `shaders/*.wgsl` (§5.5).
- **Bakes are caches** (A11: "binary only for caches"). `lasair bake` writes `.cache/geo/<hash>.lsrgeo`, keyed by a hash of the upstream network text, parameters, input files and `lasair` version. A fresh clone rebuilds them. `.lsrgeo` has a magic number and version, a sorted text header (attributes by class then name, types, counts, groups, semantics), then raw little-endian SoA blobs at 256-byte alignment and a checksum. Geometry meant to be kept leaves through Mesh File Out (glTF/OBJ/PLY) into `media/`.
- **Determinism:** wherever attributes are listed (files, headers, docs, the spreadsheet) they are sorted by (class, name). Binding order is declaration order, which is stable because it comes from the schema or the `#bind` text. Point numbering comes from deterministic ops (§4.3).
- **Round-trip tests:**
  1. Extend the existing random-network round trip (300 networks, byte-stable, in `crates/lsr-format/src/tests.rs`) to every PNT, MSH and MAT operator.
  2. `.lsrgeo`: random geometries (every type and class, zero points, empty groups, NaN payloads, ±inf, −0.0) are written, read and compared bit for bit, then written again byte-identically.
  3. Bake the same project twice and get the same bytes.
  4. Mesh File Out → Mesh File In keeps `P`, `N`, `uv` and `Cd` within the format's precision. The glTF opens in Blender (AC in §9).
  5. Damaged `.lsrgeo` and mesh files never panic (fuzz, like the existing 100,000-damaged-files test).

## 7. Windows and macOS parity (rule 6)

Read from the pinned crates, `wgpu-types 30.0.1` (`src/limits.rs`, `src/features.rs`) and `wgpu-hal 30.0.1` (`src/metal/adapter.rs`, `src/dx12/adapter.rs`):

| Limit or feature | `Limits::default()` (what we request) | DX12 adapter | Metal adapter | Design relies on |
|---|---|---|---|---|
| Storage buffers per stage | 8 | UAV-tier based: 16 on tier 2, much higher on tier 3 | 29 (31 buffer slots minus 2 internal; 9 under strict WebGPU) | **8**; request 16 when the adapter offers it |
| Storage binding size / max buffer | 128 MiB / 256 MiB | 2 GiB − 1 / `i32::MAX` | `maxBufferLength` | 128 MiB / 256 MiB; request up to 1 GiB when offered |
| Bind groups | 4 | — | — | 4 (engine group + attribute group; 2 spare) |
| Workgroup size / storage | 256 invocations / 16 KiB | — | threadgroup memory | 256 / 16 KiB |
| Workgroups per dimension | 65,535 | — | — | 65,535 (1D up to 16.7M elements) |
| Indirect draw and dispatch | core WebGPU | yes | all macOS families (`INDIRECT_EXECUTION`) | yes, single `draw_indirect`, `first_instance = 0` |
| `MULTI_DRAW_INDIRECT_COUNT` | feature | yes | **no** | not used |
| `SHADER_FLOAT32_ATOMIC` | feature | **no** | MSL 3.0+ | not used |
| `SHADER_INT64` | feature | DXC only | MSL 2.3+ | not used |
| `SUBGROUP` | feature | yes | when SIMD-scoped ops exist | optional fast path for integer scans only (same result) |
| Point size | always 1 px in WGSL | — | — | quads |

To raise the limits: in `lsr-app/src/main.rs`, `lsr-gpu/src/lib.rs` (`headless_async`) and `lsr-cli/src/render.rs`, ask for `min(adapter, target)` instead of the defaults. Every core operator must still work at the defaults. The 16 and 1 GiB targets are inferences from the hardware tiers above, not measurements. The GTX 1660 / Apple M1 guard rail (A16, PT-6) can be checked with the same limits table at startup.

## 8. Risks, my decisions, and questions for Mark

### Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Raster cost of 2M small quads is higher than estimated | PT-2 missed on the 4070 | S1 first; compute-raster fallback (§4.4) |
| MSH libraries are not deterministic in parallel | AC4 fails for projects with MSH | Check when Boolean lands (§2.3); single-threaded offline |
| A fused kernel and a node chain differ by FMA contraction | AC2 "identical" fails by an ulp | Shared WGSL library; Q4 |
| Measurements are on a 5070 Ti, not the 4070 | False confidence | Budgets scaled for bandwidth (§4.5); confirm on the lab PC |
| 8-binding floor makes wide ops awkward | More dispatches | Gather kernel (§2.1), group bitmask, raised limit where offered |
| u32 ID wrap in long shows (PT-10, 24 h soak) | IDs repeat after about 71 min at 1M/s | Documented; unique among live particles |

### Decided by me as designer (Mark may overrule any of these)
D2–D10 and D12 in the table at the top; u32 wrapping IDs; 32 groups per class; size classes as powers of two; the `.lsrgeo` cache format; the determinism rule for atomics; requesting higher limits where the adapter offers them; ownership hand-over before fusion.

### Questions for Mark, in order
1. **PT-2 definition.** The PRD does not say how particles are rendered. Accept "2-pixel additive sprites, colour from an attribute, no depth sorting" as the PT-2 bench case (`pt2.particles_2m`)?
2. **Vertex attributes deferred** (RESEARCH §2.3, never ratified in §0). Accept that UV seams and hard normals split points in M2, which affects UV Unwrap and glTF out?
3. **Bake policy.** Bakes as gitignored, rebuildable caches (A11) mean a fresh clone re-cooks slow nodes such as Fracture. Keep it that way, or add a tracked `bakes/` folder (for example under Git LFS)?
4. **AC2 wording.** Keep "identical output" as byte-identical, which forces the kernel compiler to reproduce the node chain's passes, or amend it to a stated tolerance (for example ≤1 ulp per component, or identical PNG frames)? *Depends on A12.*
5. **Hardware** (already in `M2.md`): when is the RTX 4070 lab PC available for PT-2, and which Mac measures PT-5 and the parity checks?

### Dependencies on decisions not yet ratified (PRD §0)
- **A12 (Proposed):** only §5's Kernel client, AC2 and AC3. The framework, the PNT/MSH ops and the `pnt.wgsl` route do not depend on it.
- **A25 (implemented, pending Mark's review):** §2.3 relies on `CookMode` (offline waits, `receive`, pooled outputs cleared) and on `GpuOp::status`.
- **A9, A13, A14, A20 (Proposed):** no dependency. A13's D7 (Kernel DSL) and D12 (capture pipeline) come later and fit the attribute model without changing it.

## 9. Implementation notes (2026-10-18, M2 pre-work)

What was built follows the design; this section lists only deviations, their reasons and the numbers measured. Details, commands and APIs: `docs/milestones/m2/geometry-core.md`.

### Deviations

| Where | Design | Built | Why |
|---|---|---|---|
| §3.1 static level | An `attributes` list in `OperatorSchema` | Not added; attributes are named in each operator's summary and help (and the table in `lsr-ops-pnt`'s crate docs) | `OperatorSchema` is built with struct literals in about 40 places across crates other areas are editing now; one coordinated change later (a `Default` plus a builder) avoids breaking them. |
| §3.1 runtime level | `attributes(params, input schema) → plan` on the GEO trait | `GeoOp::cook` looks attributes up itself (`Geometry::point_typed`); a missing required one is a node error naming it; missing optional ones are created (`pnt.noise` makes `v` at zero) | Nothing consumes a plan yet (no Kernel, no binding generator client); the WGSL declaration generator exists (`lsr_core::geo::wgsl_binding`). |
| §2.2 versions | A version number per attribute | None: both copies of an attribute always hold the same values. A CPU or GPU copy is added once (`OnceLock`); writing in place needs sole ownership and drops the other copy | Same guarantee with less state. |
| §3.2 hand-over | Scheduler reports why each node is planned (bitset) | `Scheduler::continuous(node)` (a reused set). The engine also hands over through **feedback** inputs (the source cooks again later in the same frame, and this is its only wire) and a node's **own** output on a self-feedback wire | Lets a simulation loop update its state in place instead of copying every frame. |
| §3.2 Solver | A `pnt.solver` node (`init`, `prev`) | No Solver node: any GEO feedback input carries state; `pnt.particles` has a `prev` feedback input and otherwise uses its own last result | Smallest version that proves state on the wire; a Solver Block (FR-P4) can wrap it later. |
| §4.2 particle passes | Update, scan, gather + emit, then curl and attract as separate deformers | Flag (`age + dt < life` on the previous state), compact (scan, index map, header), gather `P v age life id` (generic gather), emit, integrate (curl noise, attraction, gravity, drag) and `Cd` from age, **in one compute pass**; `pnt.noise` in velocity mode can be put in the loop | Fewer passes (wgpu allocates per pass, below) and less traffic: `Cd` is computed, not carried. |
| §4.1 header | "A GPU-side count" | A fixed 48-byte header on every geometry with points: count, capacity, `mapped`, `draw_indirect` and `dispatch_workgroups_indirect` arguments (`lsr_core::geo::header`); a CPU count is written once with `write_buffer` | Kernels read the count the same way whoever produced the geometry. |
| §2.1 kernels | Bind groups cached by buffer identity | Explicit bind-group layouts (`Kernel`, `Bind`) instead of shader-derived ones, so a binding a shader does not use is still bound; `BindGroupCache` shared by compute and render operators | Shader-derived layouts silently drop unused bindings. |
| §2.3 readback | Copies arrive as CPU data | `Readback::receive` (copy on this thread) and `Readback::take_mapped` (hand the mapped buffers to a worker, which copies) | Copying 2 × 1.6 MB on the render thread cost about 0.4 ms (S3 round trip). |
| §2.4 rule 2 | No allocation on the render thread after warm-up | No buffer, bind-group or texture creation after warm-up (asserted). Heap allocations are bounded per node-cook, as for IMG: 34 measured, bound 45 | wgpu allocates about 16-24 times per compute or render pass at submit and 4 per `write_buffer`; the GEO code adds about 10 per frame over six nodes (an `Arc` per new attribute generation, a `Vec` per copy-on-write clone). Recycling those is a follow-up. |
| §4.4 Render IMG | Camera unspecified | `img.render_points`: orthographic view of the XY plane (centre, height), sprite size in pixels, `Cd` or constant, add or over | "Simple orthographic/2D to start." |

### Measured (RTX 5070 Ti, DX12 unless noted, 2026-10-18; other agents were using the same GPU, so timings are noisy)

| Item | Result | Target |
|---|---|---|
| PT-2 `pt2.particles_2m` (release `lasair bench`) | frame p50 1.86 ms, p95 2.02 ms; GPU: simulate 0.75 ms, draw ≈ 1.02 ms. Vulkan: p50 1.73 ms, p95 1.80 ms, simulate 0.64 ms, draw ≈ 1.03 ms | p95 ≤ 16.7 ms on RTX 4070 (not measured there) |
| S1: 2M static sprites at 1920×1080 | GPU p50 1.01 ms, p95 1.88 ms; render-thread cook p50 0.07 ms; 0 buffers, bind groups, textures created after frame 10 | ≤ 3.0 ms, ≤ 0.3 ms, 0 |
| S2: 2M particles | simulate GPU p50 0.89 ms; simulate + draw 1.90 ms; two offline runs of 300 frames (one with 32 spinning threads): 300/300 frames identical, final `P v age life id` identical | ≤ 4.0 ms, ≤ 7.0 ms, identical |
| S3: 30 `pnt.noise` deformers over 2M points (1 copy on write, 29 in place) | GPU p50 4.53 ms, p95 5.23 ms | fusion becomes an M2 task if > 3.0 ms: **it is over** |
| S3: 100k-point GPU → CPU worker → GPU, live at 60 fps | latency exactly 2 frames; render-thread cook p50 0.31 ms, max 0.97 ms (baseline without it 0.23 ms, max 1.06 ms); `Queue::write_buffer` from the worker works | ≤ 2 frames, no stall > 0.5 ms |
| Noise library | analytic gradient vs central differences: max error 3.8e-4; divergence of the curl: ≤ 4e-3 (finite differences) | |

S3's deformer is curl noise (three gradient noises, 24 hashes per point), so the 4.5 ms is part arithmetic and part bandwidth (30 × 64 MB ≈ 1.9 GB, about 2.4 ms at the 5070 Ti's bandwidth); fusion would remove the bandwidth part. Whether to build fusion in M2 is a question for Mark (`docs/milestones/m2/geometry-core.md`).

### Deformer fusion (built 2026-10-30)

Choices where §3.2 left room, each the simplest consistent with it:

- **Which operators.** The per-point field operators (Bend, Twist, Taper, Wave; Attract,
  Vortex, Drag, Gravity; Collide, Integrate) and Transform: each reads and writes only its own
  point's `P`, `v` (created at zero by the forces) and `N`. Wind and Turbulence are left
  unfused: they call the curl-noise library, and two Winds in one Metal kernel rounded `v`
  differently from two kernels (up to 4 ulps) because the compiler treats two inlined copies
  of that large function differently. `pnt.noise` (a `Deformer` from `lsr-gpu`) and every
  operator that moves, adds or removes points end a run.
- **The planner's rule** (`lsr_gpu::engine`): a run is consecutive cooks in the frame's step
  list where each node's only wired input is the previous node's GEO output and that output
  could be handed over (its only wire, cooking this frame and next, not viewed): exactly when
  the intermediate results can never be read. Runs are at most 64 stages, and stay within one
  loop pass. The run's first node takes the input as it would; the inner nodes' results are
  released; the last node holds the result. `Engine::set_fusion(false)` turns it off;
  `Engine::fused_last_frame` counts fused nodes.
- **The fixed signature.** An operator offers `GeoOp::point_stage(params, dt)`: a kind (which
  stage function), its unfused uniforms (up to 128 bytes) and whether it writes `v`. Each kind's
  WGSL is its unfused `fn point` body, rewritten as `fn stage_<kind>(lsr_p, lsr_v, lsr_n,
  params)` over the point's local `P`, `v`, `N`. **The unfused kernels call the same function**
  between a load and a store; the fused kernel loads once, calls the stages in order with
  uniforms from an array (one block per stage) and stores once. No Kernel DSL is needed (§3.2).
- **Identical output: byte-identical, measured.** Every stage's inputs pass through an
  exclusive-or with a uniform that is always 0 (`lsr_opaque`), in the unfused kernels and
  between fused stages. Without it Metal's fast math folded one stage's arithmetic into the
  next (Twist then Taper: `(a + r) - a`), and fused `P` differed from the chain by an ulp in
  7% of words. With it the tests compare whole buffers byte for byte (Q4 answered for fusion:
  no tolerance needed). Transform became one kernel over `P`, `v`, `N` for this (it had three).
- **Pipelines** are built on a worker thread when a run's stage kinds change (`lsr-fuse`
  thread); live, the run cooks unfused until the pipeline arrives (`Engine::pending` is true
  meanwhile), and a run that could not be fused cooks entirely unfused that frame; offline the
  cook waits for it, so every frame of a render is fused alike.
- **Implementation of the stages** lives with the operators (`lsr-ops-pnt`: `fuse.rs`, the
  `PointFuser` registered with `OpLibrary::set_point_fuser`); the engine knows only stages,
  runs and fusers, so another family can register its own.

Evidence (Apple M5 Pro, Metal, 2026-10-30; other agents were building and running GPU work on
the same machine, so timings are indicative): `tests/batch/fusion.rs` — the 14-operator chain
(every fusable deformer and force, plus Wind and Turbulence, over 5,000 points with `N` and
`v`, animated) fused into two runs gives the same bytes of `P`, `v` and `N` as the unfused
chain over six offline frames; 30 Transforms and three of each deformer (42 stages) the same;
live fusion waits for its pipeline; runs stop at viewed nodes and non-stages.
`tests/alloc_batch.rs` runs with a fused Bend + Wave run: no buffer, bind-group or texture
creation after warm-up. `lasair bench` case `pnt.fusion_30_transforms_2m` (30 animated
Transforms over 2M points, frame time including GPU completion; this adapter reports zero for
GPU timestamps): fused p50 1.26-1.61 ms, unfused 8.48-9.18 ms over three runs (5.7-6.7×), `P`
byte-identical.
