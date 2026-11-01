# MSH operators: CPU meshes on worker threads (M2 design note)

**Status:** design for the M2 MSH batch, 2026-10-18. Builds on
[`geometry-attributes.md`](geometry-attributes.md) (GEO wires, residency, the S3 round trip) and
[`../spikes/msh-licences.md`](../spikes/msh-licences.md). Implementation notes and evidence:
[`../milestones/m2/msh-batch.md`](../milestones/m2/msh-batch.md).

## Decisions at a glance

| # | Topic | Decision |
|---|---|---|
| M1 | CPU data structure | Indexed polygons: positions, typed columns per point and per primitive, CSR topology (`start`, `corners`) and a closed/open flag per primitive. The same shape as the GEO `Topology`, so conversion is a copy. No half-edge structure: operators that need adjacency (Normals, Subdivide, Extrude, PolyReduce) build an edge map for the cook and drop it. |
| M2 | Where cooks run | A shared pool of worker threads (2 to 4, 64 MiB stacks for the BSP recursion). One job is a pure function of (parameter snapshot, input meshes). Each job runs single-threaded, so results do not depend on the number of threads. |
| M3 | Hand-over to the worker | On the render thread the node takes a **snapshot** of its parameters (into a reused buffer: no allocation when nothing changed) and **handles** to its input geometries (cheap clones). Inputs whose attributes have a CPU copy go straight to the job. GPU-only attributes (a PNT source) are read back first with `Readback` (`take_mapped` hands the mapped buffers to the worker, which copies them); a GPU-counted input also reads its header for the live count. |
| M4 | Hand-back | The worker converts the result to a `Geometry`, uploads the point attributes and a header itself (`Queue::write_buffer` from the worker, as S3 measured), keeps the CPU copies (residency *both*), and sends it back on a channel. Topology stays on the CPU (no GPU consumer needs it yet). The render thread only swaps the value in. |
| M5 | Caching and what recooks | A node submits a job only when its key changes: the snapshot differs (bitwise for numbers) or an input is a different value (attribute handles compared with `Attribute::shares`). Otherwise it keeps its output; the engine already caches results and propagates dirtiness. |
| M6 | Live behaviour | The node outputs its last finished result (empty before the first) and sets `cook_again` while work is in flight; the status line says "cooking…" or the last cook time. |
| M7 | Offline behaviour | (`CookMode::Offline`, `docs/render.md`): the cook waits. A readback flushes the frame's encoder (so the upstream GPU work of this frame runs first), maps and waits; the job's result is received with `CookMode::receive`. Frame *i* uses the inputs of frame *i*. Every MSH operator is classed **Waits**. |
| M8 | Stale cooks | Latest wins, one job in flight per node plus at most one waiting. A newer key replaces the waiting job (it never runs) and sets the cancel flag of the running job, which stops at its next check (`Job::check`, every few thousand elements) and sends nothing. While a slider is dragged, a running job is left to finish when the node has delivered nothing for 250 ms, so the view keeps updating. Deleting the node cancels its job. |
| M9 | Memory bounds | Every operator checks the size of its output **before** allocating (`limits`: at most 8M points, 16M primitives and 32M corners per mesh; Iso Surface at most 256 cells per axis; Subdivide refuses levels whose estimated output exceeds the limits). File readers check every declared count against the bytes that remain in the file before allocating, refuse files over 512 MiB, and are fuzzed with damaged files under a counting allocator that fails on any single allocation over 64 MiB. |
| M10 | Libraries | None added. Every algorithm is implemented in `lsr-ops-msh` (pure Rust, MIT OR Apache-2.0), so `cargo deny` is unchanged and no C/C++ toolchain is needed. Manifold (`manifold-csg-sys` clones at build time) and meshoptimizer (vendored C) remain options for later, behind the same operator schemas. |
| M11 | Determinism | No `HashMap` iteration decides output order; random numbers are a hash of (seed, index); floating-point work is sequential. Two runs give the same bytes (tests compare them). |

## Data model in detail (M1)

```
Mesh
├─ p: Vec<[f32; 3]>                     positions (P)
├─ point_attrs: [Column]                N, Cd, uv, pscale, id, group, ...
├─ start: Vec<u32> (prims + 1), corners: Vec<u32>     polygon/polyline topology (CSR)
├─ closed: Vec<bool>                    polygon (true) or polyline (false)
├─ prim_attrs: [Column]                 per-primitive values (N, group, ...)
└─ point_groups, prim_groups: names     bit i of the class's `group` column is group i
Column { name, comps (1..4), values: F32 | I32 | U32 (tightly packed) }
```

Positions are `f32` in storage; algorithms that need it (Boolean, PolyReduce) work in `f64`.
3-component columns are padded to 16 bytes only when converted to GEO attributes (layout rules,
design §2.1). Attribute propagation goes through one helper (`Interp`) that copies or blends
float columns by weights and takes the heaviest source for integer columns, so every operator
carries user attributes through without knowing their names.

**No vertex attributes** (design D1/Q2): UV seams and hard edges split points. Primitives are
therefore welded closed manifolds with smooth normals and no `uv` (Grid keeps `uv`); Normals in
`flat` mode or with a cusp angle splits points for hard edges; UV Project writes `uv` per point.

## Cook protocol (M3-M8)

```
render thread (cook)                                  worker pool
────────────────────────────────────────────────────  ─────────────────────────────
collect finished results (try_recv; offline: wait) ◄── send (generation, Geometry | error)
snapshot params into reused buffer
key changed? ── no ─► keep output
   │ yes
   ├─ inputs on the CPU? ─► queue job (latest wins) ──► convert inputs → Mesh, run,
   └─ GPU-only attributes:                              convert → Geometry, upload P/N/…
        record Readback (+ header copy) in this frame        and header (write_buffer)
        after_submit: start mapping
        later cook: take_mapped ─► queue job with Mapped ─► worker copies mapped bytes
cook_again while anything is in flight
```

Offline the same steps run in one cook: flush the encoder, wait for the mapping, run the job,
wait for its result.

## Algorithms chosen (and why)

| Operator | Algorithm | Notes and limits |
|---|---|---|
| Subdivide | Catmull-Clark | Works on quads and n-gons, which the primitives produce; Loop needs triangles. Boundary edges and points use the crease rules. Float attributes are interpolated linearly (face, edge and vertex points). |
| PolyReduce | Quadric edge collapse (Garland & Heckbert 1997) on triangles | Optimal position from the 3×3 quadric system, with midpoint fallback; rejects collapses that flip a face or break the link condition; boundary points can be locked; stops at the target share of triangles or when the cheapest collapse costs more than the tolerance (a distance). |
| Boolean | BSP-tree CSG (Thibault & Naylor 1987) in `f64` with a plane epsilon | Robust enough for closed, modest meshes (up to 200k polygons per input). Output polygons are welded by position; T-junctions can remain where splits meet, so the result is closed visually but not always watertight by index. Coplanar faces are handled by the BSP rules (kept once). No exact predicates. |
| Iso Surface | Marching tetrahedra (each grid cell split into six tetrahedra around its main diagonal, the same split everywhere) | No ambiguous cases and no lookup tables: closed fields give closed manifold meshes of triangles and quads (vertices shared on grid edges). Surface nets were tried first and dropped: saddle faces made non-manifold edges on blended point clouds. Field: smooth union of spheres around input points (radius × `pscale`), or an analytic SDF (sphere, box, torus, gyroid). Normals from the field gradient. |
| Scatter | Area-weighted triangle sampling with a cumulative table and `sqrt` barycentrics | Seeded hash per point index, so the same seed gives the same points; writes `N` and interpolates point attributes. |
| Sweep | Parallel-transport frames along the curve | Rotation-minimising, no flips at inflections; scale and twist along the curve; caps when the profile is closed. |
| Normals | Area-weighted face normals (Newell) | Cusp angle splits points where adjacent faces differ by more than the angle. |
| Triangulate | Ear clipping in the polygon's dominant plane | Fans for convex polygons fall out of the same code; degenerate polygons are dropped. |

## Files (Mesh File In/Out)

Small readers and writers written here: OBJ (`v`, `vt`, `vn`, `f`, `l`, `g`/`o` as primitive
groups, vertex colours), PLY (ASCII and binary, little- and big-endian), glTF 2.0 (`.glb` and
`.gltf` with embedded or relative buffers; static meshes: triangles, lines and points; node
transforms of the default scene; `POSITION`, `NORMAL`, `TEXCOORD_0`, `COLOR_0`, including
normalised integer components). Not read: skins, morph targets, animations, sparse accessors,
Draco or meshopt compression (an error names the extension). Writers are deterministic (no
timestamps); glTF output is triangulated and carries `min`/`max` as the specification requires.
