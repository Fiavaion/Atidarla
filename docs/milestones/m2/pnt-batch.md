# M2 wave 2: the PNT operator batch

**Status:** built ahead of M2's formal start at Mark's request (2026-10-18), for review, on the
geometry core ([`geometry-core.md`](geometry-core.md)). PRD §9 M2 AC5 asks for PNT ≥ 30
operators "including particles, forces, Solver Block, For-Each and a deformer stack": there are
now **34 PNT operators** (31 new) plus line drawing in Render Points. Nothing relies on a PRD
decision still marked Proposed; the open points are under "Questions". Design notes for Solver
and For-Each: [`docs/design/solver-foreach.md`](../../design/solver-foreach.md).

## What was built

Every operator ships complete (CLAUDE.md rule 7): schema with a cook budget, a generated page in
`docs/operators/pnt/`, a GPU test with expected values (`crates/atd-ops-pnt/tests/batch/`), use
in an example project (a unit test checks every PNT operator appears in `examples/`), and an
offline class in `classify()` (`crates/atd-cli/src/render.rs`, `docs/render.md`).

| Group | Operators | Where |
|---|---|---|
| Forces on `v` (reading `P`) | Attract (spring, constant, inverse-square; falloff), Vortex (swirl, pull, lift), Drag (linear, quadratic), Gravity, Wind (relaxation to a wind velocity, curl-noise gusts), Turbulence (fractal curl noise) | `src/field.rs` |
| Simulation steps | Collide (plane, outside or inside a sphere; bounce, friction), Integrate (`P += v dt`), Age (`age += dt`; removes the dead, points in or out of a box or sphere, slow points) | `src/field.rs`, `src/select.rs` |
| Sources | Scatter (line, circle, disk, rectangle, sphere surface, ball, box; `N` on surfaces), Emitter (from a source's points into a particle state, in its loop) | `src/sources.rs`, `src/emitter.rs` |
| Structure | Merge (four inputs, attribute union), Copy to Points (pscale, orient or N, colour, `copynum`), Delete (group, box, sphere, attribute comparison; Invert = split), Subset (every Nth, seeded fraction, range; by `id`), Group (bits of `group` from any rule; replace, union, intersect, subtract), Sort (axis, distance, attribute, random, reverse) | `src/sources.rs`, `src/select.rs`, `src/sort.rs` |
| Deformers | Transform (with `v`, `N`), Bend, Twist, Taper, Wave (linear, radial) | `src/transform.rs`, `src/field.rs` |
| Attributes (any type) | Attribute (constant, ramp, index, noise), Attribute Math (19 operations, A op B), Randomize (uniform, normal, direction; by `id` or a key attribute), Colour by Attribute (ramps into `Cd`) | `src/attr.rs` |
| Neighbours (GPU grid hash) | Normals (covariance, closed-form smallest eigenvector), Smooth (weighted average, iterations), Connect (k nearest as two-point polylines, `near`, `neardist`) | `src/neighbours.rs` |
| Loops and pieces | Solver (loop entry: `init`, `prev` feedback; a Pack around it is a Solver Block), Piece (For-Each Begin: `piece` by chunks, attribute buckets or grid cells) | `src/solver.rs` |
| Render Points (IMG) | Draw: points, lines (polyline primitives as a line list) or both | `src/render.rs` |

Offline classes: Stateful: Emitter, Solver (with Particles); every other new operator is Pure.

**Shared pieces** (`src/kit.rs` and friends), which the remaining PNT operators and MSH/MAT can
reuse:

- `PointKernel`: a compute kernel over a geometry's live points, dispatched from its header
  (works for CPU and GPU counts alike); `writable` (in place when held uniquely, else a pooled
  copy) and `readable`; runtime-typed attribute access in WGSL (`typed_access`, `desc`), so one
  pipeline serves `float`, `vec2`-`vec4` and `int` attributes.
- `Compact` (Delete, Subset, Age): keep flags → integer scan → index map → gather of every
  point attribute, GPU-counted result.
- `Sorter`: bitonic sort of (key, index), stable and deterministic, stage parameters in one
  uniform buffer at dynamic offsets (no bind groups per frame).
- `Grid`: the neighbour grid hash on top of `Sorter` (cell hash sort, start/end table, 27-cell
  query in a fixed order).
- `work_header`: operators that write a new header each frame write and bind a fixed buffer of
  their own and copy it into the pooled output header (see "for other areas").

**Examples** (all four render with bloom or composite at 1280×720):

| Project | Shows |
|---|---|
| `examples/pnt-fountain.atidarla` | Emitter from a Scatter ring into a Particles loop: Gravity, Vortex, Turbulence, Wind, Drag, Collide; Colour by speed |
| `examples/pnt-deformers.atidarla` | Twist, Delete (a window), Taper, Bend, Wave on a column; Randomize, Group, Delete, Subset, Copy to Points, Transform, Merge |
| `examples/pnt-constellation.atidarla` | Scatter sphere, Noise, Smooth, Normals, Colour (N.z), Sort, Connect drawn as lines under sprites; a Subset star field |
| `examples/pnt-solver.atidarla` | Attribute (noise into `v`), Piece, Randomize keyed by piece, Solver with Attract, Turbulence, Drag, Integrate, Age; Attribute Math |

## Evidence (actually run, 2026-10-18, RTX 5070 Ti, Windows 11, DX12)

Other agents were compiling and running GPU work on the same machine, so timings are noisy.

| Check | Result |
|---|---|
| `cargo test -p atd-ops-pnt` | lib 6, `alloc` 1, `alloc_batch` 1, `batch` 24, `ops` 6 passed; spikes and the fusion bench ignored |
| `cargo test --workspace` | 443 passed, 0 failed, 6 ignored (the four spikes, the fusion bench, one existing) |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` | clean |
| `cargo run -p atd-cli -- docs --check` | up to date |
| `cargo run -p atd-cli -- check --gpu --allow-python-ops examples/*.atidarla` | 25 of 25 ok (including the four new ones) |
| `atidarla render` twice, frames 0-120, SHA-256 per frame (release) | pnt-fountain, pnt-deformers, pnt-constellation, pnt-solver: 120/120 identical each, 120 distinct frames each; the core's particles and points (Particles changed, see below) still 120/120 |
| `atidarla bench` (release), after the Particles change | `pt2.particles_2m` p50 1.87 ms, p95 2.05 ms, GPU simulate 0.80 ms, simulate + draw 1.80 ms, no creations after warm-up (core: 1.86 / 2.02 / 0.75 / 1.78 ms); `pt1.img_chain_4k` p50 5.56 ms |
| Rule 2, `tests/alloc_batch.rs`: four animated chains (particle loop with Emitter and five forces; deformer stack with Delete, Attribute Math, Randomize; Noise, Normals, Connect with lines; a Solver loop with Attract, Integrate, Age), 24 node-cooks per frame, 100 frames after 40 of warm-up | 0 buffers, 0 bind groups, 0 textures created; 40.0 heap allocations per node-cook (bound 45, the geometry core's; nearly all wgpu's per pass and per `write_buffer`) |
| S3 follow-up (`tests/fusion.rs`, release): 30 Transform deformers over 2M points | GPU p50 1.67 ms, p95 1.94 ms unfused; the same stack composed into one Transform (what fusion would dispatch; outputs agree to 1.6e-6): p50 0.15 ms, p95 0.21 ms |
| Same run: the geometry core's 30 curl-noise deformers over 2M points | GPU p50 4.19 ms, p95 4.69 ms (4.53 / 5.23 ms in the core's run) |

What the GPU tests check, in short: forces add the known acceleration × step (exact values);
Collide moves points onto the plane or sphere and reflects `v` by Bounce; Integrate; Bend's
half circle lands the end 4/π above the start; Twist, Taper and Wave on known points;
Transform with pivot and `v`; Delete, Subset and Group give the expected ids in order, also on
a GPU-counted particle state; Age's rules; Scatter's points lie on their shapes (ball: mean
radius 0.75, uniform in volume); Merge's union with zero fill and GPU-counted inputs; Copy to
Points with pscale, colour and N; Emitter appends in emission order and births spread over the
step; Sort orders, carries attributes and is a seeded permutation; Normals of a sphere (within
14°) and of a tilted plane (exact); Smooth's weights on a step; Connect's corners and the lines'
pixels; Solver integrates, resets, starts at Start Frame; a Solver Block Pack flattens and gives
the top-level loop's result; Piece and Randomize keyed by piece.

## Decisions taken here (smallest versions; Mark may overrule)

- **Split by group** is Delete with Invert (an operator has one output today); two Deletes split.
- **Delete by expression**: per-point expressions wait for the Kernel DSL (A12); Delete compares
  an attribute component with a value (`<`, `<=`, `>`, `>=`, `==`, `!=`).
- **Merge and Copy to Points drop primitives** (points only): mesh-aware versions belong with MSH.
- **Surface scatter** (on a mesh) is a hook, not built: it needs triangle areas, a prefix sum
  over them (the existing `Scan`) and a binary search per point; it lands with MSH meshes.
- **Neighbour bound**: Max Neighbours (default 128 for Normals and Smooth) bounds the work per
  point; past it the first found (in a fixed order) are used, which biases the result.
- **Solver and empty states**: the Solver restarts from `init` when `prev` has no geometry; a
  state whose live count reached zero on the GPU is not seen (that would need a readback).
- **Fixed work headers** (above): the gathers outgrew their 32-entry bind-group caches when
  many operators write a new pooled header each frame; binding a fixed header removes the
  pool's order from the cache keys, at the cost of one 48-byte copy per operator and frame.
  Applied to Compact, Sort, Merge, Copy to Points, Emitter and the geometry core's Particles
  (its output is unchanged: the core's tests, examples and PT-2 numbers above).

## Not done

- Solver sub-steps and For-Each iterations (designed in `solver-foreach.md`, question 1-2).
- Deformer fusion in the planner: measured above (1.67 → 0.15 ms for 30 transforms; the noise
  stack is half arithmetic); a planner rewrite that composes straight runs of Transforms (or
  joins per-point kernels) needs the engine, not this crate.
- Per-piece parameters other than a key attribute (Kernel DSL).
- The static `attributes` list in `OperatorSchema` (as the geometry core says: one coordinated
  change); attributes are named in each operator's summary and help.
- An app viewer for GEO nodes (view through Render Points), a 3D camera in Render Points.
- Measurements on the RTX 4070 lab PC and on a Mac (Metal parity of vertex-stage storage reads
  for the line drawing, dynamic uniform offsets, the closed-form eigen solver).

## Questions for Mark

1. **Solver sub-steps** by flatten-time unrolling of a Solver Block Pack and a `dt` scale in the
   geometry's detail values: build in M2, or is one step per frame enough for now?
2. **For-Each** as "a `piece` attribute plus operators keyed by it" until the Kernel DSL: accept
   as M2's For-Each?
3. **Fusion**: 30 cheap deformers cost 1.7 ms at 2M points (under the 3 ms threshold); composing
   them would cost 0.15 ms. Worth an engine-side fusion of Transform runs in M2, or later?
4. **Split** as Delete + Invert, and **Delete by attribute comparison** instead of expressions:
   enough for M2?
5. **Render Points lines** (one-pixel polylines): keep in Render Points, or move to the 3D
   render operator the MAT area is building?

## Proposed PRD decision-log rows (IDs at merge)

| ID | Decision | Rationale |
|---|---|---|
| PNT-1 | PNT operators that change the point count (Delete, Subset, Age, Merge, Copy, Emitter) give GPU-counted results when any count is only on the GPU, and never read a count back. | Rule 1; compaction by an integer scan keeps order and determinism (GEO-4). |
| PNT-2 | The Solver Block is a Pack around `pnt.solver` (init + feedback); sub-steps, if built, unroll at flatten time. | State stays on the wire (D6); no container type in the engine. |
| PNT-3 | For-Each in M2 is a `piece` attribute (Piece) and operators keyed by an attribute (Randomize, Group, Delete, Sort, Colour); per-piece parameters wait for the Kernel DSL. | Design §3.2 "once over all elements with the piece index as an attribute"; one dispatch per operator. |
| PNT-4 | Operators that write a new header each frame bind a fixed header of their own and copy it to the pooled output header. | Bind-group caches keyed by pooled headers thrash when many operators rotate headers (measured: 6 bind groups a frame before, 0 after). |

## For the other areas

- **Pooled headers and bind-group caches**: a kernel that binds a header (or any buffer) taken
  from the pool every frame can outgrow its 32-entry `BindGroupCache` once a graph has many such
  operators. Bind a buffer of your own (`work_header` pattern) where you can.
- **Names this batch writes**: `group` (u32 bits, `Geometry::groups`), `piece`, `copynum`,
  `near`, `neardist`, `N` (Scatter, Normals), `pscale`/`orient` (read by Copy to Points).
- **Connect writes topology on the GPU** (`__kind`, `__start`, `__corners` attributes without
  CPU copies): an MSH operator reading it must read it back (`readback`).
- **Merge and Copy to Points are point-only**; an MSH Merge needs to offset corner indices.
- **Shared glue touched**: one block of `classify()` arms in `crates/atd-cli/src/render.rs`,
  the PNT lines of `docs/render.md`'s class table, four rows of `examples/README.md`; nothing in
  `atd-gpu` or `atd-core`.
