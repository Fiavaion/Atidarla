# M2 pre-work: MAT operators and the 3D renderer (Render IMG)

**Status:** built ahead of M2's formal start at Mark's request (2026-10-18), for review, in parallel with the PNT and MSH operator batches. Covers the PRD §9 M2 items "MAT ≥ 6; Render IMG" (PRD §6: "Render (GEO + MAT + camera)", "MAT (≈8): PBR, Unlit, Matcap, Toon, Point Sprite, Line, Splat, WGSL Material"). Nothing here relies on a PRD decision still marked Proposed; the open questions are at the end. Builds on the geometry core ([`geometry-core.md`](geometry-core.md)).

## What was built

| # | Item | Where |
|---|---|---|
| 1 | **MAT wires in the engine.** A `Material` value (shading model, a fixed 96-byte uniform block, the node feeding its texture), a MAT results map, a `MatOp` trait and `MatCtx`, `OpLibrary::add_mat`, `CookCtx::mat_inputs` (`MatInputs::get`, `MatInputs::texture`), `Engine::material`. The shared WGSL (`mat::WGSL`, `mat::SHADE`). | `crates/atd-gpu/src/mat.rs`, `crates/atd-gpu/src/engine.rs` |
| 2 | **Six MAT operators** (PRD names): `mat.unlit`, `mat.pbr`, `mat.matcap`, `mat.toon`, `mat.line`, `mat.point_sprite`. | `crates/atd-ops-mat/src/materials.rs` |
| 3 | **`img.render`**: up to four geometry + material pairs (`geo`/`mat` … `geo4`/`mat4`) in one depth buffer; orbit or look-at camera, perspective or orthographic (target, azimuth, elevation, distance or position; field of view, ortho height, near, far); one directional light plus ambient; cull back, none or front; 4× MSAA; wireframe overlay; point size for geometry without primitives. | `crates/atd-ops-mat/src/render.rs`, `camera.rs` |
| 4 | **GPU draw records** from CSR topology: polygon fans with edge flags (wireframes skip the fan's diagonals), polyline segments; per-primitive count, integer scan, a header with indirect arguments, one invocation per record finding its primitive by binary search. Validated on the CPU; rebuilt only when the topology or point count changes. | `crates/atd-ops-mat/src/triangulate.rs` |
| 5 | **`msh.platonic`** (Platonic Solid): the five solids and regular polygons, faceted (`P`, `N`, `uv` split per face; optional prim `Cd`; centre, radius). The renderer's test and example source until the MSH primitives land (see "MSH at merge"). | `crates/atd-ops-mat/src/platonic.rs` |
| 6 | Bench case `render.mesh_1m`; three examples; generated pages; offline classes (all Pure); registration in the app and CLI. | `crates/atd-cli/src/bench.rs`, `examples/render-*.atidarla`, `docs/operators/{img/render,mat/*,msh/platonic}.md`, `crates/atd-cli/src/render.rs`, `docs/render.md` |

The task's material list maps onto the PRD's names: Constant/Unlit, Textured and Vertex-colour are **Unlit** (colour × `Cd` × `map` at `uv`); Lambert/Phong/Standard and Emissive/Glow are **PBR** (albedo, roughness, metallic, emission × strength; `map`, `Cd`); Wireframe is **Line**; Toon and Matcap as named; **Point Sprite** draws points. Splat (FR-C1, M6) and WGSL Material (with FR-K5) are not built.

## The MAT contract (how renderers and later operators share materials)

- **The wire.** `WireType::Mat` already existed (PRD §5.4 "material handle"); the engine now cooks a node whose first output is MAT through `MatOp` into a `Material` (plain data, no GPU objects, no heap) and keeps it per node like IMG and GEO results. Dirty propagation and caching are unchanged.
- **Textures are looked up, not held.** A MAT operator's first IMG input is its texture. The engine records the source node in `Material::texture_source`; a renderer asks `MatInputs::texture(material)` for that node's *current* image when it draws. A material therefore never keeps a pooled texture that has since been released and reused (which would happen when the image cooks while the material does not, for example while only the image is viewed).
- **One shading function.** `atd_gpu::mat::SHADE` defines `shade(m, n, v, l, light, ambient, cd, texel)` switching on the model ID; `mat::WGSL` is the uniform struct (`colour`, `emission`, `colour2`, `params`, `pbr`, `flags`). A renderer prepends both, so every renderer shades a model the same way.
- **Adding a model:** a `Model` variant with the next ID, a branch in `shade()` reading only the struct's fields, and a MAT operator that fills them. Models needing a different draw (Splat: sorted Gaussians; Point Sprite already) also need a pipeline in the renderer. A WGSL Material or a Kernel-built one would bring its own `shade()`; the renderer would then build one pipeline per distinct shader (on a worker thread, as `img.wgsl` compiles). Not built.

## Renderer notes

- **Attributes read:** point `P` (vec3, required), `N` (vec3; else the triangle's own normal, faceted), `Cd` (vec4 point, else vec4 prim), `uv` (vec2), `pscale` (f32, sprites). Wrong types count as missing. Vertex pulling from the attribute buffers (no vertex buffers), as Render Points; CPU-only attributes (MSH results) are uploaded once (`Attribute::ensure_gpu`).
- **Conventions** (question 4): right-handed, Y up; the camera looks down its −Z; polygons are front-facing when counter-clockwise seen from the front; `uv` has v up (0 at the bottom of the image, as OpenGL and Houdini), so the renderer samples `(u, 1 − v)`; colours are not converted (display-referred, as the rest of IMG).
- **Hostile geometry** never reaches the GPU as an out-of-bounds access: the `start` list must have prims + 1 entries (else a node error), at most 8,388,608 primitives, record lists must fit one storage binding and 65,535 workgroups (else an error naming the counts); on the GPU a primitive whose range runs backwards or past the corner list is skipped, a corner at or beyond the point count makes its record degenerate, counts are clamped to the buffers' capacity; in the vertex shader a triangle with any non-finite position or clip coordinate is dropped whole, segments and sprites behind the camera or non-finite are dropped. A missing `P` is a node error.
- **Determinism:** no atomics anywhere; records are in primitive order, slots draw in order, opaque drawing with depth "less" (ties keep the first fragment); antialiasing is MSAA with coverage turned into a fixed sample mask (`@builtin(sample_mask)`, no blending), so nothing depends on GPU scheduling. Alpha below 1 becomes screen-door transparency (with MSAA, 4 levels); there is no sorted transparency.
- **Rule 2:** pipelines are made at creation (per sample count, on first use); MSAA and depth targets once per output size; uniforms written only when they change; bind groups cached (`BindGroupCache` for buffers, a small cache for textures). Steady frames create no buffers, bind groups or textures (tested; bench asserts it). Heap allocations per frame were not counted with an allocator (the PNT alloc test pattern would do it).
- **Rule 1:** everything is recorded into the frame's encoder; `msh.platonic` builds at most 256 points on the CPU on parameter edits only.

## Evidence (actually run, 2026-10-18, RTX 5070 Ti, Windows 11, DX12)

Two other agents were compiling and running GPU work on the same machine throughout, so every timing is noisy.

| Check | Result |
|---|---|
| `cargo test -p atd-ops-mat` | 12 passed (5 unit: schemas, camera maths ×2, platonic ×2; 7 GPU: exact pixels of an unlit square with and without MSAA; depth independent of draw order and culling; PBR, Toon and Matcap values against hand-computed ones; textures upright through `uv` (render equals the ramp image within 1/255); Line edges without diagonals, filled and unfilled, a polyline, a pixel-size sprite, the wireframe overlay; hostile topology (bad indices, empty, 2-corner, 1-point, unknown kind, backwards and past-the-end ranges), NaN and ∞ positions, a malformed `start` length, a 100,000-corner polygon, no primitives, no `P`; byte-identical frames across two engines and no GPU objects created in steady frames) |
| The same 7 GPU tests with `WGPU_BACKEND=vulkan` | 7 passed |
| `cargo test --workspace` | 425 passed, 0 failed, 5 ignored (includes the MAT layout unit test in `atd-gpu`). `atd-cli`'s `python_operators_render_deterministically_only_when_allowed` failed twice when run on its own earlier (a `ValueError` from the example's `ops/*.py` declarations) and passed in this full run; no Python or `atd-py` file was changed here, Python 3.11 is first on `PATH` (the project expects 3.13), and the test copies the example into the shared temp folder that other agents also use. Worth a look, not caused by this work as far as I can tell. |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all` | clean |
| `atidarla docs --check` | up to date |
| `atidarla check --gpu --allow-python-ops examples/*.atidarla` | 24 of 24 ok (including the three new `render-*`) |
| `atidarla render` twice (release), frames 0-120, SHA-256 per frame | render-solids 120/120 identical (120 distinct), render-textures 120/120 (120 distinct), render-wire-toon 120/120 (120 distinct); the same with the debug build before the last example edit |
| `atidarla bench` (release), `render.mesh_1m`: 1,002,528 triangles (708 × 708-quad sphere, smooth normals), PBR, 4× MSAA, depth, back-face culling, 1920×1080, orbiting camera, 300 frames; the mesh covers 45% of the image | DX12, three runs: p50 0.43 / 0.36 / 0.38 ms, p95 0.57 / 0.51 / 0.55 ms, max 1.54 / 1.01 / 1.43 ms; GPU p50 0.34 / 0.32 / 0.35 ms; render-thread cook 34-46 µs. Vulkan: p50 0.35 ms, p95 0.46 ms, GPU 0.34 ms, cook 10 µs. First frame (shader compile, upload of 16 MB of attributes and topology, GPU triangulation) 131-311 ms, once per topology. No buffers, bind groups or textures created after warm-up (asserted). Same runs: PT-1 p50 5.6 ms, PT-2 p50 1.89 ms. |

**Looked at by eye** (frames written by `atidarla render` into the worktree's `.scratch/`): render-solids frame 60: a faceted dodecahedron with a colour per face, darker on the faces turned away from the light (upper right), and an orange icosahedron whose facets differ in brightness and glow through Bloom; edges antialiased. Render-textures: the square shows the colour noise; the matcap dodecahedron is light on faces towards the camera and dark blue on a face seen edge-on, as a radial ramp should give. Render-wire-toon: three cel bands on the icosahedron, the dodecahedron's wireframe with its back edges and no fan diagonals, the point cloud as small lit spheres hidden correctly behind the icosahedron. An early version of the solids example saturated the emissive solid to flat yellow (emission strength up to 1.8 plus Bloom); the example now uses 0.2-0.8. No shading bugs seen.

## MSH at merge (how the MSH agent's operators plug in)

- MSH operators output a `Geometry` with CPU attributes (`P`, `N`, `uv`, `Cd`) and CSR topology (`PrimKind::Polygon` for faces, `Polyline` for curves). Nothing else is needed: Render uploads what it reads once and triangulates on the GPU when the topology's buffers change (a new `Topology` value per cook means new records per cook; measured as the bench's first frame).
- For correct pictures, MSH should wind faces counter-clockwise seen from outside (else use `cull = none`), write `N` as a point `vec3` (hard edges split points, design D1, Q2), `uv` with v up, `Cd` as `vec4`.
- Examples here use `msh.platonic`; at merge they can switch to `msh.box`/`msh.sphere` etc. if Mark prefers (question 6). Registration glue touched: one line each in `crates/atd-cli/src/project.rs` (`library`), `crates/atd-app/src/app.rs`, two `classify` lines in `crates/atd-cli/src/render.rs`, one cell in `docs/render.md`, three rows in `examples/README.md`, a new case in `crates/atd-cli/src/bench.rs`. `atd-ops-mat` now depends on `atd-gpu` and `wgpu`.
- For the PNT agent: Render draws any GEO, including GPU-counted particles (Point Sprite or no primitives: `draw_indirect` from the header), in perspective, with depth against meshes.

## Not done

- A camera and lights from CMP operators (PRD §6 CMP Camera, Light, Environment): Render has parameters only. One directional light; no shadows, no environment lighting, no tone mapping or colour management.
- Sorted transparency (alpha is screen-door), instancing (Copy to Points), vertex (corner) attributes, prims on GPU-counted geometry beyond their capacity, mipmaps for minified textures (they alias), more than four slots per Render.
- Splat and WGSL Material; a material preview in the app (viewing a MAT node shows nothing).
- A heap-allocation count for Render (only GPU-object creation is checked).
- **Metal / macOS parity is not checked** (rule 6): the renderer uses vertex-stage storage reads (as Render Points), `@interpolate(flat, either)` and a `sample_mask` fragment output; all are core WebGPU, but nothing here ran on a Mac.

## Questions for Mark

1. **MAT contract** (rows MAT-1, MAT-2 below): accept materials as CPU plain data with a fixed uniform block, textures looked up by source node, and one shared `shade()` over model IDs?
2. **The six MATs** are the PRD's PBR, Unlit, Matcap, Toon, Line and Point Sprite, with "Textured", "Vertex-colour" and "Emissive" as parameters of Unlit and PBR rather than operators. Agreed?
3. **Camera and lights as parameters** until CMP lands; then Render gains `camera` and `light` CMP inputs that override them?
4. **Conventions** (row MAT-3): counter-clockwise front faces, cull back by default, `uv` v up, Y-up right-handed world, no colour conversion?
5. **Antialiasing and transparency:** 4× MSAA with coverage as a sample mask, screen-door alpha, no sorting: enough for M2?
6. **`msh.platonic`:** keep it as an MSH operator (it is not in the PRD's MSH list) or retire it when the MSH primitives merge and move the examples to them?
7. **Four slots per Render** (one depth buffer); more objects mean several Renders composited (no shared depth) until a CMP scene exists. Enough?

## Proposed PRD decision-log rows (for Mark to accept, amend or reject; IDs to be assigned at merge)

| ID | Decision | Rationale |
|---|---|---|
| MAT-1 | A `MAT` value is CPU plain data: a shading-model ID, a fixed uniform block and at most one texture, which is the material operator's first IMG input and is looked up by source node when drawing; shading is one shared WGSL `shade()` over model IDs; a WGSL or Kernel material brings its own `shade()` with a pipeline per shader. | No GPU objects or allocation per material; every renderer shades alike; a material can never hold a recycled pooled texture. |
| MAT-2 | Render IMG draws GEO through draw records built on the GPU from CSR topology (polygon fans with edge flags, polyline segments; integer scan and binary search, no atomics) with vertex pulling, depth "less", draws in slot and primitive order, and MSAA whose coverage is a fixed sample mask (no blending). Topology sizes are validated on the CPU; bad indices and non-finite positions are dropped on the GPU. | Byte-identical offline renders (FR-H1) and no GPU fault from hostile geometry; measured 120/120 identical frames on three examples. |
| MAT-3 | Geometry conventions for rendering: right-handed Y-up world, camera looking down −Z, counter-clockwise front faces, `uv` with v up, `N` point normal else faceted, `Cd` point else prim, colours not converted. | MSH, PNT, importers and the renderer must agree; these match glTF's winding and OpenGL/Houdini's uv (glTF importers flip v). |
