# M2 pre-work: MSH operator batch

**Status:** built ahead of M2's formal start at Mark's request (2026-10-18), for review, in
parallel with the PNT batch and the MAT/3D-render work. Design:
[`docs/design/msh-operators.md`](../../design/msh-operators.md). Builds on the geometry core
([`geometry-core.md`](geometry-core.md)). Covers the MSH part of M2 AC5 (PRD §9: "MSH ≥ 18 core
operators" and the PNT → MSH → PNT chain). Nothing here relies on a PRD decision still marked
Proposed; questions are at the end.

## What was built

| Item | Where |
|---|---|
| **CPU mesh model**: indexed polygons and polylines (CSR like the GEO topology), typed point and primitive columns, groups as bits, size limits, attribute propagation (`Interp`), lossless conversion to and from `Geometry` | `crates/atd-ops-msh/src/mesh.rs` |
| **Cook protocol** (`MeshNode`, the `GeoOp` of every MSH operator): parameter snapshot into a reused buffer, change detection (snapshot plus attribute identity of the inputs), a shared pool of 2-4 worker threads (64 MiB stacks), GPU-only inputs read back with `Readback`/`take_mapped` (plus the header of GPU-counted inputs), results converted and **uploaded on the worker** (point attributes and header, residency *both*), latest-wins with one job running and one waiting, cancellation flag checked in long loops, a starvation guard (a superseded job finishes if nothing was delivered for 250 ms), panics turned into node errors, status line ("cooking…", "cooked in N ms"). Offline: the frame's encoder is flushed, the readback and the job are waited for. | `crates/atd-ops-msh/src/cook.rs` |
| **26 operators** (below), each with schema, help on every parameter, render-thread budget 100 µs, generated docs page, use in an example, tests, offline class | `crates/atd-ops-msh/src/*.rs`, `docs/operators/msh/` |
| **File formats** written here: OBJ, PLY (ASCII, binary LE/BE), glTF 2.0 (.gltf with data URIs or relative files, .glb) readers and writers; counts checked against the bytes left before allocating; files over 512 MiB refused | `crates/atd-ops-msh/src/io.rs` |
| **Four example projects** visible through Render Points | `examples/mesh-*.atidarla` |
| Registration: `atd_ops_msh::library` in `atd-cli` `project::library` and the app; `classify()`: every `msh.*` is **Waits** (one prefix rule); `docs/render.md` Waits row | glue, one line each |

Operators (26; the PRD asks for at least 18):

| Group | Operators |
|---|---|
| Primitives (7) | `msh.grid`, `msh.box`, `msh.sphere` (polar, ico), `msh.tube` (cylinder, cone), `msh.torus`, `msh.circle` (polygon, outline, points, arc), `msh.line` |
| Files (2) | `msh.file_in` (OBJ, PLY, glTF/GLB), `msh.file_out` (OBJ, PLY, glTF, GLB) |
| Edits (8) | `msh.normals` (smooth with cusp angle, flat, prim), `msh.triangulate` (ear clipping), `msh.transform` (TRS about a pivot; normals by the inverse transpose; mirrors flip polygons), `msh.merge` (4 inputs, attributes and groups by name), `msh.delete` (by group, points or prims, invert), `msh.group` (box, normal, range, all; replace/union/intersect/subtract), `msh.fuse` (weld by distance), `msh.colour` (constant, normal, uv, position, random per point or per prim) |
| Modelling (4) | `msh.subdivide` (Catmull-Clark), `msh.extrude` (individual with inset, connected regions; groups `extrude_front`/`extrude_side`), `msh.uv_project` (planar, cylindrical, spherical, box), `msh.polyreduce` (quadric edge collapse, target share and tolerance, locked borders) |
| Advanced (5) | `msh.boolean` (union, intersect, subtract; BSP in f64), `msh.sweep` (profile or circle along a curve; parallel transport; scale, twist, caps), `msh.skin` (loft through profiles; ring, caps), `msh.scatter` (seeded, area-weighted; `N`, `id`, interpolated attributes, optional `pscale`), `msh.iso_surface` (marching tetrahedra; blended point spheres × `pscale`, or sphere, box, torus, gyroid) |

### Choices (and why)

- **No mesh library was added.** Every algorithm is implemented in the crate, so `cargo deny` sees no new dependency (the only new edges in `Cargo.lock` are workspace crates and `serde_json`/`wgpu`, already in the workspace) and no C/C++ build is needed. The spike's allowed libraries stay options: Manifold's `-sys` crate clones at build time (refused by the spike), meshoptimizer would need a vendored C build. Writing the file readers here also means every byte of a hostile file goes through code that is bounded and fuzzed.
- **Catmull-Clark** over Loop: the primitives produce quads and n-gons; Loop needs triangles.
- **Marching tetrahedra** for Iso Surface: surface nets was implemented first and dropped when the blended-point test showed non-manifold edges at saddle faces; tetrahedra have no ambiguous case.
- **Boolean by BSP** (Thibault & Naylor 1987, the csg.js formulation): honest limits below.
- **Welded primitives** (design D1, no vertex attributes): closed shapes share points, have smooth `N` and no `uv` (Grid has `uv`); hard edges come from Normals (flat or cusp), UVs from UV Project, OBJ seams split points by default.

## Evidence (actually run, 2026-10-18, RTX 5070 Ti, Windows 11, DX12)

Other agents were compiling and running GPU work on the same machine, so timings are noisy.

| Check | Result |
|---|---|
| `cargo test -p atd-ops-msh` | 42 passed, 0 failed, 2 ignored (timings): unit 7, primitives 12, modelling 10, files 5, fuzz 2, engine (GPU) 6 |
| Render thread, live at 60 fps, a Transform of a 130k-point sphere recooking every frame (time expression; release) | Transform node avg 46 µs (budget 100 µs), max 614 µs; whole frame p50 0.19 ms, p95 0.74 ms, max 1.27 ms; results keep arriving (starvation guard) |
| `cargo test --workspace` | 454 passed, 0 failed, 6 ignored (the 4 spikes, 1 existing, 1 MSH timing) |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` | clean |
| `atidarla docs --check` | up to date (26 MSH pages) |
| `atidarla check --gpu --allow-python-ops examples/*.atidarla` | 25 of 25 ok (including the four new `mesh-*`) |
| `cargo deny check` | **not run**: cargo-deny is not installed on this machine; no external dependency was added |
| `atidarla render`, frames 0-120, twice, SHA-256 per frame (debug build) | mesh-primitives 120/120 identical (120 distinct images); mesh-modelling 120/120 (114 distinct); mesh-scatter-iso 120/120 (120 distinct); mesh-files 120/120 (1 distinct: static) |
| AC5 chain test (GPU): sphere → Scatter → Iso Surface → `pnt.noise` → Render Points, offline, two engines | topology rides through the GPU deformer (P copied on write, prims kept), pixels lit, frames change with time, the two runs byte-identical |
| Live behaviour tests | first cook returns at once with no result and no error; the worker's result is on the GPU (`Residency::Both`) with a header; unchanged inputs start no job; a GPU-only PNT input is read back (≥ 2 frames) and matches; a parameter changed every frame for 40 frames ends on the last value; an error clears when the parameter is fixed |
| Offline test | frame *i* uses frame *i*'s parameters through a GPU-only PNT input (flush, readback, wait); nothing is left in flight |
| Fuzz (`tests/fuzz.rs`) | 5000 damaged OBJ/PLY/glTF/GLB files: 337 read (all valid meshes), 4663 refused, no panic, no single allocation over 64 MiB (counting allocator); hostile counts (10⁸ PLY vertices, 4·10⁹ glTF elements) refused with under 1 MiB allocated |
| Round trips | OBJ, glTF, GLB: `P`, `N`, `uv`, `Cd` exact; PLY: exact except 8-bit colour (≤ 0.51/255); writers byte-identical across runs |
| Worker cook times (release, best of 3) | polar sphere 256×512 (131k quads) 3.4 ms; Subdivide box 5 levels 0.9 ms; Normals cusp 30° on it 31 ms; PolyReduce to 10% 398 ms; Boolean ico (5120 triangles) minus box 156 ms (425 ms before removing allocations from the BSP loop); Scatter 1M points 84 ms; Iso Surface 2000 points at resolution 128 (506k prims) 264 ms; Extrude every polygon 47 ms; GLB write 15 ms, read 12 ms; Mesh→GEO 1M points 9.4 ms, GEO→Mesh 12.8 ms |

Commands: `cargo test -p atd-ops-msh`; `cargo test -p atd-ops-msh --release --test timings -- --ignored --nocapture` (worker times and the render-thread cost); `python .scratch/determinism.py target/debug/atidarla.exe 0-120 mesh-primitives mesh-modelling mesh-scatter-iso mesh-files` (a throwaway script, not committed: two renders, SHA-256 per frame).

## Limits, stated honestly

- **Boolean**: inputs should be closed; output is welded by position but can keep T-junctions where splits meet (closed visually, not always by index; the box-box tests are exact); work grows roughly with the square of the triangle count for convex inputs (inputs capped at 200,000 triangles; 5k triangles take about 0.15 s); no exact predicates; normals are not written (add Normals). Single-threaded, so the TBB determinism question of the design (§2.3) does not arise.
- **PolyReduce**: triangles only (polygons are triangulated first), loose points dropped, polylines kept with their points locked; the tolerance is approximate (sum of squared plane distances).
- **Extrude**: inset only in individual mode; connected mode moves points along the mean normal of their selected polygons (sharp corners of a region get thinner walls).
- **Sweep/Skin**: one curve and one profile (first primitive of each); Skin needs equal point counts; `uv` of closed profiles has the usual seam.
- **Files**: no skins, morph targets, animations, sparse accessors, Draco or meshopt (refused by name); glTF output is triangulated, one mesh; PLY output drops polylines; OBJ materials ignored; File In does not watch the file (reload by changing the path).
- **Topology stays on the CPU** after the worker uploads point attributes: no GPU consumer needs it yet. The MAT/3D renderer will need triangle indices (see below).
- **Rule 2 (render thread)**: an unchanged MSH node allocates nothing (snapshot in place, identity comparison). A node whose key changes allocates a handful of small values per change (the snapshot and input handles for the job, the task box), as an edit does. Not yet measured with the counting allocator.

## Not done

- Lazy upload (upload only when a GPU consumer reads the value): every MSH result is uploaded now, which costs VRAM for MSH-to-MSH chains.
- A counting-allocator test for the render-thread side, a `.atdgeo` bake, File In reload on file change, multi-curve Sweep, Boolean with exact predicates or Manifold, a Remesh operator (spike: Geogram needs Mark's sign-off).
- Viewing meshes as meshes: examples draw vertices through `img.render_points`. **At merge time** the examples can be upgraded to the MAT/3D renderer (swap `img.render_points` for the mesh renderer; the GEO wires stay).

## For the other areas

- **MAT/3D render**: an MSH result has point attributes on the GPU (`P`, `N`, `Cd`, `uv`, … uploaded by the worker) and a header, but its **topology (`__kind`, `__start`, `__corners`) is CPU-only**. Polygons may be n-gons and polylines. To draw triangles, triangulate on the CPU (`atd_ops_msh::edit::triangulate_polygon` is public) or call `Attribute::ensure_gpu` on the topology and triangulate on the GPU. Normals are point normals; flat shading needs `msh.normals` mode `flat` (split points).
- **PNT**: MSH outputs are ordinary GEO values with a CPU header (`Geometry::set_points(n, Some(header))`), so `Deformer` works on them (copy on write; topology shared). PNT outputs feed MSH nodes directly; GPU-counted (particle) inputs are read up to their live count.
- **Glue conflicts**: `classify()` gained one `starts_with("msh.")` rule; `project::library` and the app each gained one `atd_ops_msh::library` line; `docs/render.md` one sentence in the Waits row; `examples/README.md` four rows.

## Questions for Mark

1. **No external mesh library for M2**: accept in-crate implementations (BSP Boolean, QEM PolyReduce, own OBJ/PLY/glTF readers) for M2, with Manifold/meshoptimizer as later swaps behind the same schemas? Manifold needs a vendored `-sys` crate first (spike).
2. **Boolean robustness**: is "closed visually, T-junctions possible, ~n² cost" acceptable for M2, or should exact-predicate CSG (or Manifold) be a milestone requirement (PT-12 mentions Boolean performance)?
3. **Upload policy**: upload every MSH result eagerly (now; simple, render thread never copies) or only when a GPU consumer reads it (saves VRAM in MSH chains, but the first GPU read then copies on the render thread)?
4. **Welded primitives without `uv`** (consequence of design Q2, no vertex attributes): acceptable, or should primitives offer a "split seams with uv" option?
5. **Render-thread allocations on change**: accept "none when unchanged, a few small ones per changed cook" for MSH nodes under rule 2, like the GEO bound?

## Decision-log rows: ratified 2026-10-30 (Mark accepted; IDs assigned, now in PRD §0)

| ID | Decision | Rationale |
|---|---|---|
| A34 (was MSH-1) | MSH operators are pure functions of (parameter snapshot, input meshes) run on a shared worker pool; live, a node shows its last finished result and cooks again until the newest is in (latest wins, one running and one waiting, cancellation by flag); offline, a node flushes the frame's GPU work, reads back, and waits. | Rule 1 and FR-H1 together: renders are byte-identical (four examples, 120/120 frames) while the live render thread never waits. |
| A35 (was MSH-2) | The worker uploads MSH results (point attributes and header) itself; topology stays on the CPU until a GPU consumer needs it. | `Queue::write_buffer` from a worker works (S3); the render thread only swaps a value, so a 1M-point result costs it nothing. |
| A36 (was MSH-3) | Mesh file readers are written in-crate and bounded: counts checked against the bytes left before allocating, files over 512 MiB refused, fuzzed with a 64 MiB per-allocation cap. | Mesh files are untrusted input (same policy as the media fuzz tests); no new dependency licences to vet. |
