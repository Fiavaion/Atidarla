# Tír (working title) — PRD.md

**Version:** 0.1 (draft, all decisions Proposed) · **Date:** 28 October 2026 · **Owner:** project owner (TBC)
**Companion:** `tir/RESEARCH.md` v0.1. Section references are written `R§n`.
**Execution:** milestone by milestone (M0–M6). Each milestone ends only when every one of its acceptance criteria passes and is recorded in `tir/docs/milestones/Mn.md`.

> **Tír** (Irish for *land*) is a free, open-source node-based terrain generator for Apple Silicon, with a path to NVIDIA/CUDA. It sends heightmaps live into Unreal Engine 5.8 Landscapes. Every preset in it is a graph that students can take apart to learn how node-based workflows work.

---

## 0. Decision log

Status key:
- **Ratified**: decided by the owner.
- **Proposed**: recommended by this PRD. Needs a yes/no before M0 closes.

| # | Decision | Status | Evidence |
|---|---|---|---|
| T1 | Working title **Tír**; project file `*.tir/` (folder of text files); CLI `tir`. The title is a placeholder until a trademark check is done | Proposed | — |
| T2 | Separate product from Atidarla, same owner. Shares its licence policy, its text-project conventions and (if ready) its egui node canvas | Proposed | R§5.3 |
| T3 | Licence **MIT OR Apache-2.0**. No GPL or non-commercial code in the app or the UE plugin. Hesiod (GPL-3.0) is reference only | Proposed | R§1.6, R§4.5 |
| T4 | **Platform priority:** macOS on Apple Silicon (M1+, macOS 15+) is the primary target. Windows + NVIDIA (CUDA) is the second target from M5. Linux + NVIDIA comes cheaply with the CUDA backend but is best-effort | Proposed | Brief; R§5 |
| T5 | **Host language Rust** (edition 2024). Thin compute HAL with backends: `metal` (`objc2-metal`, Metal 4), `cuda` (`cudarc`), `wgpu` (Vulkan/DX12 fallback). Viewport in `wgpu`. UI in `egui` 0.37 | Proposed | R§5.2–5.3 |
| T6 | **Kernel language Slang**, single source. Compiled offline and at build time to MSL (Metal) and CUDA/PTX, with WGSL as fallback. Escape hatch: hand MSL/CUDA kernels, each with a Slang reference and a parity test. **Gated by the M0 bake-off**; if Slang→MSL fails the gate, use CubeCL | Proposed | R§5.2 |
| T7 | **Canonical data:** f32 heightfields in metres, stored in GPU buffers (not textures), shared-storage zero-copy on Apple Silicon. Textures are viewport views only. No FP64 anywhere on the hot path | Proposed | R§5.1 |
| T8 | **World-unit, time-based parameters.** Distances are in metres, erosion in years or "age", never pixels or iterations. Preview and build resolution are independent | Proposed | R§3.4 |
| T9 | **UE bridge = C++ editor plugin + TCP/shared-memory stream** writing into a **LandscapeTexturePatch** on a named procedural edit layer. Fallback 1: `FLandscapeEditDataInterface` into a named edit layer. Fallback 2: a Python script (no compiled plugin). File export always available | Proposed | R§4.4 |
| T10 | **UE support window:** UE 5.8 primary, 5.7 best-effort. UE 6 Early Access is evaluated when available | Proposed | R§4.1 |
| T11 | **Learn-first UX is a core requirement, not an add-on:** Recipes, Simple ⇄ Graph reveal, Replay, Predict cards, Wire-it-up challenges, Stack view | Proposed | R§6 |
| T12 | **Construction log** stored in the project (ordered, append-only, compactable). It powers Replay and makes graph-building diffable | Proposed | R§6.2 |
| T13 | ML nodes are **optional**, run in-process through Core ML / Metal tensors (Mac) or ONNX Runtime + CUDA (NVIDIA). Weights are never bundled; a model manager records licence and source | Proposed | R§3.3 |
| T14 | No telemetry, no accounts, works fully offline. Crash reports are opt-in and written locally first | Proposed | Atidarla A17 precedent |
| T15 | No competitor trademarks or competitor node names in product UI. Comparisons appear only in docs | Proposed | Atidarla rule 10 precedent |

---

## 1. Problem and vision

**The problem has four parts.**
- **Mac users are underserved.** The reference node-based terrain tool, Gaea, is Windows-only, including the 3.0 Early Access. Mac users and Mac-based classrooms make do with fewer options (R§2).
- **The Unreal link is the hardest part.** Every terrain tool reaches UE by exporting files. Students then fight Z-scale maths, odd valid resolutions and destructive re-imports (R§2.4).
- **No terrain tool is built to teach.** Node graphs are a big conceptual step for students who have only used layer-based or GUI tools. Today they cross it through YouTube.
- **Classroom licensing is a hurdle.** It is per-seat or capped, with resolution limits on free tiers.

**Vision.** One open application:
- It feels like a simple slider app the first time a student opens it.
- It reveals the node graph behind every slider the moment they ask "how is this made?".
- It replays how the graph was built, node by node.
- It pushes the result live into the Unreal level they have open, at the right scale, without destroying their sculpting.
- It runs best on the Macs students own, and the same graph builds on NVIDIA GPUs.

---

## 2. Users and personas

1. **First-year student (primary).**
   - Has used Photoshop-style layers and maybe UE sculpt tools. Has never used a node graph.
   - Typically on a MacBook Air or Pro (M1–M5, 8–32 GB).
   - Needs to be productive in 10 minutes and understand the graph in an hour.
2. **Educator.**
   - Needs assignments that can be distributed as files, reproducible results, offline operation, no licence administration, and Git-friendly projects.
   - Wants to see *how* a student built something (construction log).
3. **Environment artist / technical student.**
   - Builds large worlds for UE 5.8 with World Partition.
   - Needs high-resolution builds, tiling, weightmaps, a live link and batch builds.
4. **Indie developer.**
   - Wants a free alternative with no caps.
   - May work on Windows + NVIDIA (from M5).
5. **Contributor / tool developer.**
   - Writes new nodes in Slang.
   - Needs clear node schemas, tests and docs.

---

## 3. Goals and non-goals

**Goals (v1.0)**
- G1. A node-based heightfield generator with about 60 nodes covering primitives, noise, combining, shaping, erosion and hydrology, masks, colour/splat, import and export (§6).
- G2. Interactive preview on base Apple Silicon. High-resolution builds up to 16k² in one piece on 64 GB+ Macs, with tiled builds beyond that.
- G3. A live, non-destructive, bidirectional UE 5.8 Landscape bridge working in metres (§7.5).
- G4. A Learn layer that turns presets into lessons (§7.4).
- G5. The same graph builds on NVIDIA/CUDA within numeric tolerance (§8).
- G6. Free, open, uncapped, offline.

**Non-goals (v1.0)**
- Full 3D terrain (overhangs, caves). Possible later via UE 5.8 Mesh Terrain (horizon).
- Real-time in-app game rendering, foliage scattering or PCG. Hand off to UE PCG instead.
- In-app sculpting beyond simple masks and brushes. "Paint a mask" is in scope; a full sculpt suite is not.
- Unity, Godot or Houdini bridges in v1.0. File export covers them; bridges are horizon.
- Matching every Gaea node. Aim for parity on the classic looks, not feature count.
- Cloud features, accounts, marketplaces.

---

## 4. Differentiators

| ID | Differentiator | Milestone |
|---|---|---|
| X1 | Learn-first UX: Recipes, Simple ⇄ Graph, Replay, Predict, Wire-it-up, per-node mini-examples, Stack view | M4 (foundations in M1–M2) |
| X2 | Live, edit-layer-aware, metres-native, bidirectional UE 5.8 bridge | M3 |
| X3 | Apple-Silicon-native engine: unified-memory single builds, zero-copy, Metal 4 dispatch | M1–M2 |
| X4 | Free and open, no caps, no classroom licence | all |
| X5 | Single-source kernels with a CUDA build | M5 |
| X6 | World-unit, time-based parameters for consistency between preview and build | M2 |
| X7 | Text project format + construction log | M1 |

---

## 5. System architecture

### 5.1 Process model

- **One app process.**
  - UI thread: egui.
  - Viewport: wgpu.
  - **Cook scheduler:** CPU worker pool that encodes GPU work.
  - **GPU queue(s):** Metal 4 `MTL4CommandQueue` or a CUDA stream.
- **The UI never waits on a cook.** Cooks are cancellable. A parameter drag cancels and restarts the downstream cook at preview resolution.
- **Builds** (full resolution or tiled) run as background jobs with progress and cancel. `tir build` runs the same engine headless.
- **The UE bridge** runs on its own thread with a bounded send queue. A slow UE never stalls the app.

### 5.2 Crates (Cargo workspace)

| Crate | Responsibility |
|---|---|
| `tir-core` | Graph model, node schema registry, cook scheduler (lazy pull, dirty propagation, cancellation), caches, undo/redo, construction log |
| `tir-hal` | Compute HAL trait (buffers, kernels, dispatch, sync, timing) + backends `metal`, `cuda`, `wgpu` |
| `tir-kernels` | Slang sources + build script (slangc → MSL / PTX / WGSL), reflection → typed Rust bindings |
| `tir-nodes` | Node implementations (schema + kernel calls + CPU reference for tests) |
| `tir-project` | `*.tir/` text format (TOML/JSON with stable IDs, deterministic ordering), migrations, round-trip tests |
| `tir-io` | PNG16 / R16 / RAW / EXR / TIFF / GeoTIFF read-write, metadata sidecars, DEM import with attribution |
| `tir-ue` | Bridge protocol (client side), UE size/scale maths, tiling and naming rules |
| `tir-viewport` | wgpu terrain renderer (displaced grid, LOD, hillshade, overlays, water, splat preview) |
| `tir-ui` | egui shell, node canvas, Simple view, Stack view, Learn layer |
| `tir-learn` | Recipes, lesson scripts (Predict / Modify / Wire-it-up), replay engine |
| `tir-ml` | Optional: model manager, Core ML / Metal tensor runner, ONNX Runtime + CUDA runner |
| `tir-app` / `tir-cli` | Binaries: GUI app; `tir build | bench | lesson-check | serve-bridge` |
| `ue-plugin/TirBridge` | UE 5.8 C++ editor plugin (separate build, source under MIT OR Apache-2.0) |
| `ue-plugin/python` | Zero-install Python fallback script |

### 5.3 Data types on wires

| Type | Storage | Meaning | Wire colour (colour-blind-safe palette) |
|---|---|---|---|
| **Height** | f32 buffer, metres | Elevation | blue |
| **Mask** | f32 buffer, 0–1 | Selection or weight | grey |
| **Colour** | RGBA16F buffer | Albedo / splat preview | orange |
| **Vector** | 2×f32 | Flow direction, warp offsets | purple |
| **Bundle** | named set of the above | Multi-output nodes (e.g. erosion outputs height + sediment + wear + flow) | striped |

- Every buffer carries **extent metadata**: world size in metres, origin, resolution and the **seed space**. Nodes are resolution-independent functions of world space.
- **Implicit conversions are limited to Height → Mask** (normalise, with a visible badge). Everything else needs an explicit node, so students see the conversion.

### 5.4 Cook model

- **Pull-based.** The viewport and the outputs request a node at a target resolution and extent; only dirty ancestors re-cook.
- **Caching.** Per-node cache keyed by (parameter hash, input hashes, extent, resolution), with an LRU byte budget. On Apple Silicon the default budget is 25 % of `recommendedMaxWorkingSetSize`.
- **Preview resolution** adapts to the hardware (512² to 2048²). The build resolution is user-set.
- **Simulation nodes** expose *age*, not iterations. The node maps age to steps per resolution, so a 1024² preview and a 8192² build approximate the same physical time.
- **Tiled builds** compute each tile with a halo equal to the maximum reach of the graph, summed over the reach declared by each node, then crop. Nodes that cannot tile are flagged in the schema and force a full-extent pre-pass.
- **Determinism.** The same graph, inputs and backend give byte-identical output. Random streams are seeded by world coordinate plus node seed. Accumulations use fixed-point integer atomics or gather patterns.

### 5.5 Project format (`*.tir/`)

```
my-island.tir/
  project.toml          # metadata, units, default extent, UE target, attribution list
  graph.toml            # nodes (stable IDs), params, wires; deterministic order
  promoted.toml         # Simple-view slider panel: labels, ranges, bindings
  history.jsonl         # construction log (append-only; compactable)
  lessons/*.toml        # optional: Predict / Modify / Wire-it-up scripts (Recipes)
  assets/               # imported images/DEMs (or references)
  cache/                # binary caches; git-ignored
```

- Round-trip tests run for every format change.
- Format versions migrate forward automatically.

---

## 6. Node library (v1.0 target ≈ 60)

Names are our own (T15). There is a **Beginner palette** of about 15 nodes, marked ★. The full palette is unlocked from a menu at any time; the choice is never forced.

| Family | Nodes |
|---|---|
| **Shapes** | Plane ★, Cone/Mountain ★, Ridge Line, Crater, Dome, Slope Ramp, Island Falloff ★, Spline/Path Shape, Paint Mask ★, Image In ★ |
| **Noise** | Fractal Noise (fBm) ★, Ridged Noise ★, Billow, Cells (Worley) ★, Warp (domain warp) ★, Simplex/Periodic, Dunes (horizon v1.x) |
| **Combine** | Blend ★ (add/sub/max/min/multiply/screen), Layer With Mask ★, Math Expression, Switch, Choose By Mask |
| **Shape modifiers** | Remap/Curve ★, Terrace ★, Clamp, Blur, Sharpen, Erode-by-Min (morphology), Stretch/Transform, Tile/Mirror, Normalize to Metres |
| **Simulate** | Hydraulic Erode ★ (pipe model, multi-layer), Thermal Erode ★, Rain Erode (deterministic particle), Uplift & Erode (stream power), Age Erode (analytical, time slider), Amplify (multi-scale), Fill Sinks, Rivers (v1.x), Snow (v1.x) |
| **Masks / selectors** | Height Select ★, Slope Select ★, Curvature, Flow / Wetness, Sediment / Deposits, Aspect (facing), Distance-to-Edge, Occlusion |
| **Colour / surface** | Gradient Map ★, Splat Mixer (up to 8 paint layers → UE weightmaps), Hillshade, Satellite Look (preset ramps) |
| **Data** | DEM Import (GeoTIFF/HGT, attribution), UE Landscape In (bridge), Height Probe |
| **Output** | Export Height (PNG16/R16/EXR/TIFF + sidecar), Export Weightmaps, Export Mesh (OBJ/glTF, decimated), **Send to Unreal ★** |
| **Utility** | Group/Macro (promote params), Portal (named send/receive), Reroute, Note, Seed, Bypass (any node) |
| **ML (optional, T13)** | Diffuse Terrain (Terrain Diffusion, MIT weights), Upscale DEM (TerrainSR, Apache-2.0), Sketch-to-Terrain (horizon) |

**Every node ships complete:**
- schema;
- a docs page with a plain-language sentence ("This node lets water carve valleys");
- a **runnable mini-example** opened from right-click (R§6.2 OP Snippets precedent);
- a Predict card;
- a CPU reference implementation for tests;
- a unit test;
- a cook-time budget at 1024².

---

## 7. Functional requirements

IDs are stable. "MUST" means required for that milestone's acceptance.

### 7.1 Graph editor
- **FR-G1** Node canvas: pan/zoom at 60 fps with 500 nodes; typed coloured wires; search-to-add (fuzzy; also matches plain-language synonyms such as "rain", "cliff", "river"); multi-select; copy/paste; groups/macros; comments; minimap; auto-layout. (M1)
- **FR-G2** **Live thumbnail on every node**, on by default. Thumbnails show hillshade for Height, greyscale for Mask and colour for Colour. **Solo** (view this node in the viewport) by click; **Bypass** on every node. (M1)
- **FR-G3** Parameter panel generated from the schema. Units are shown (m, °, years). Each parameter has a one-line help text and a reset control. (M1)
- **FR-G4** **Value probe:** hovering the viewport shows the height in metres, slope and mask value at that point. Hovering a wire shows a min/max/histogram tooltip. (M1)
- **FR-G5** **Cook visualisation** (toggle): cook-order numbers on nodes, a wire pulse when data flows, and per-node cook time. (M2)
- **FR-G6** Undo/redo for every action. Each action is also appended to the construction log (T12). (M1)
- **FR-G7** Promote any parameter to the Simple view with a friendly label, range and units. (M1)

### 7.2 Viewport
- **FR-V1** 3D terrain view: GPU-displaced grid with LOD, orbit/fly camera, sun direction, hillshade/slope/flow/mask overlays, water plane, splat colour preview, scale reference (a 1.8 m figure and a 100 m grid). (M1)
- **FR-V2** 2D view with a contour-line overlay and a measurement tool. (M1)
- **FR-V3** Side-by-side compare: A/B two nodes or two parameter states. (M4)

### 7.3 Simulation and build
- **FR-S1** Erosion and hydrology nodes per §6, with world-unit and age parameters (T8). Each has a "preview fidelity" badge if preview and build are known to diverge. (M2)
- **FR-S2** Build dialog: target resolution with a picker offering **UE-valid sizes** (505 … 8129 and World Partition multiples); single or tiled; per-tile halo computed automatically; progress, cancel, ETA. (M2)
- **FR-S3** Single-piece builds up to 16,384² on Macs with ≥64 GB. The app checks memory before building and suggests tiling when memory is short. (M2)
- **FR-S4** `tir build project.tir --res 8129 --out dir/ [--tiles 4x4] [--set param=value]` runs headless and deterministically. Variable overrides support batch "wedges". (M2)
- **FR-S5** "Build preview": render the selected node once at full resolution in a background job, to show how preview and build differ. (M2)

### 7.4 Learn layer (X1)
- **FR-L1 Recipes.** At least 20 shipped presets, each a real graph with promoted sliders, a construction log and a lesson script.
  - Coverage: Rolling Hills, Alpine Range, Volcanic Island, Desert Mesas, River Valley, Coastal Cliffs, Canyon, Glacial Valley (approx.), Archipelago, Real-World DEM + Erosion, and others.
  - They are graded **Starter / Explorer / Builder** by graph size: ≤8, ≤20 and ≤40 nodes. (M4; 5 Recipes by M2)
- **FR-L2 Simple ⇄ Graph.**
  - A Recipe opens in **Simple view**: slider panel plus viewport.
  - **"How is this made?"** animates the reveal of the graph, with every slider visibly bound to its node parameter.
  - Hovering a slider highlights its node and the path from that node to the output. (M4)
- **FR-L3 Stack view.**
  - Any graph that is a single chain can be shown as a **layer stack**, the mental model from Photoshop and World Creator. Stack and graph are two views of the same document.
  - Adding a branch in the graph shows the stack "unfolding" into a graph, with an explanation. This is the bridge from GUI/layer thinking to node thinking. (M4)
- **FR-L4 Replay.**
  - A scrubber over the construction log replays the graph node by node, with captions written by the Recipe author and optional narration text.
  - Play, step, and speed 0.5–2×. Each node's thumbnail appears as it is added.
  - Works on any project, so students can replay *their own* work and teachers can replay submissions. (M4)
- **FR-L5 Predict cards (PRIMM).**
  - Lesson steps ask a multiple-choice prediction ("What will bypassing Thermal Erode do?"). The student commits an answer, then the app performs the change and shows before/after.
  - No grading by default. An optional self-check score is kept locally. (M4)
- **FR-L6 Modify tasks.** Guided steps such as "Make the mountains twice as tall", "Add snow only above 2,000 m" and "Swap Ridged Noise for Cells". Each is checked by a lesson rule on the graph or parameters, with a hint ladder. (M4)
- **FR-L7 Wire-it-up (Parsons).** A Recipe variant with its wires removed (optionally with distractor nodes); the student reconnects it. The check compares output similarity, not wire identity, so equivalent solutions pass. (M4)
- **FR-L8 Per-node help.** Right-click → "Show me": opens the node's mini-example graph in a side canvas, with a one-sentence explanation and before/after thumbnails. (M2 for ★ nodes, M4 for all)
- **FR-L9 Glossary bridge.** Each concept is linked to its GUI-tool equivalent (e.g. "Blend Max ≈ Lighten layer mode", "Hydraulic Erode ≈ UE Landmass Erosion brush") and to its UE Landscape counterpart. (M4)
- **FR-L10 Fading scaffolds.** Learn overlays can be dismissed per lesson and globally ("I know this"); they never block the canvas, following expertise reversal (R§6.1). (M4)
- **FR-L11 Classroom pack.**
  - Educators author lessons as text (`lessons/*.toml`).
  - `tir lesson-check` validates that a lesson's checks pass on its reference solution.
  - Students submit a `*.tir/` folder or zip that includes `history.jsonl`. (M4)

### 7.5 Unreal Engine bridge (X2)
- **FR-U1 Plugin.** `TirBridge` is a UE 5.8 editor plugin (C++, Win + Mac; 5.7 best-effort). It listens on a configurable port. The pairing code is shown in both apps, and the bridge is local-network only by default. (M3)
- **FR-U2 Push (live).** The **Send to Unreal** node streams height (and optional weightmaps) whenever its input re-cooks, rate-limited to one push in flight. The plugin writes into a `LandscapeTexturePatch` bound **by name** to a landscape and a procedural edit layer (default name `Tír`). The patch is created if it does not exist. Height is encoded in world units, so the student's sculpt layers stay untouched. (M3)
- **FR-U3 Scale handshake.**
  - The app sends extent and height range in metres. The plugin either creates a landscape with the correct XY/Z scale, or maps the push onto an existing landscape's transform and reports any clipping.
  - **Students never enter a Z scale.** The UI shows "1 m in Tír = 1 m in Unreal".
  - The maths follows R§4.3 and is unit-tested against Epic's worked example (4207 m → Z ≈ 821.68). (M3)
- **FR-U4 Resolution.** If the landscape's vertex count differs from the push, the app resamples on the GPU before sending. When creating a new landscape it proposes the nearest UE-valid size. (M3)
- **FR-U5 Weightmaps.** Splat Mixer outputs map to UE paint layers by name, through weight patches (one per paint layer) or the edit-data fallback. (M3)
- **FR-U6 Pull.** The **UE Landscape In** node reads the current merged height (or a chosen layer) from UE into the graph. Students can sculpt in UE, erode in Tír, and push back to a layer above. (M3)
- **FR-U7 Bake.** The plugin can bake the live patch into a persistent edit layer and disconnect, mirroring Houdini Engine's cook-layer → baked-layer workflow (R§2.3). (M3)
- **FR-U8 World Partition.** Tiled push for World Partition landscapes. Tiles stream in priority order around the editor camera. File export uses UE tile naming `name_x{i}_y{j}`. (M3)
- **FR-U9 Fallbacks.**
  - (a) `FLandscapeEditDataInterface` writes into a named edit layer, used when the patch path is unavailable.
  - (b) **Zero-install Python script** for lab machines without compiled plugins. It watches an export folder and calls `landscape_import_heightmap_from_render_target` into a named edit layer.
  - (c) Manual export of PNG16/R16 + JSON sidecar, with a printed UE import cheat-sheet (Z scale pre-computed). (M3)
- **FR-U10 LAN.** The same protocol works from a Mac to a Windows UE workstation on a LAN. Transfers are compressed with lossless zstd and sent as 16-bit or f32 tiles. (M3)
- **FR-U11 Mesh Terrain.** UE 5.8 Mesh Terrain (TextureModifier) target. (Horizon spike)

### 7.6 Import / export
- **FR-I1** Export height as PNG16, R16/RAW, EXR (f32), TIFF/GeoTIFF; weightmaps as PNG8/16; mesh as OBJ/glTF; and a JSON sidecar (extent, metres, UE Z/XY scale, attribution). (M1–M2)
- **FR-I2** DEM import (GeoTIFF, SRTM HGT) with reprojection to a local metric grid, NODATA repair and an automatic attribution block (R§3.5). (M2)
- **FR-I3** Image import as height or mask, with explicit bit-depth warnings (8-bit → terracing explanation). (M1)

### 7.7 ML nodes (optional)
- **FR-M1** Model manager: download on demand, licence badge, source URL, hash. Nothing is bundled (T13). (M6)
- **FR-M2** Diffuse Terrain and Upscale DEM. On Mac they run via Core ML / Metal tensors, using the Neural Accelerators on M5 and newer. On NVIDIA they run via ONNX Runtime CUDA. On unsupported hardware the node is hidden, with an explanation. (M6)

### 7.8 Headless and tooling
- **FR-H1** `tir build` (FR-S4), `tir bench` (writes §8 results as JSON), `tir lesson-check`, `tir serve-bridge` (headless push for CI tests against UE). (M2–M4)
- **FR-H2** Signed + notarised macOS DMG (universal not required: arm64 only). Windows installer from M5. (M5–M6)

---

## 8. Performance and quality targets

Measured by `tir bench` on the reference machines. A regression of more than 10 % fails CI on the self-hosted Mac runner.

| ID | Target | Reference hardware |
|---|---|---|
| PT-1 | Slider drag on a non-simulation node → updated viewport ≤ 100 ms at 1024² preview, on a 20-node graph | M1 8 GB (guard rail) |
| PT-2 | Hydraulic Erode (pipe) at 1024², "medium age" preset ≤ 1.5 s; Thermal ≤ 0.3 s | M1 8 GB |
| PT-3 | Same as PT-2 ≤ 0.4 s / ≤ 0.1 s | M5 Pro |
| PT-4 | Reference graph "Alpine Range" (≈25 nodes, 2 erosions) full build at 8129² ≤ 60 s | M5 Pro 48 GB |
| PT-5 | Same graph at 16,384² single piece ≤ 4 min, peak memory ≤ 40 GB | M5 Max 128 GB |
| PT-6 | UE bridge push of 2017² height → visible in UE viewport ≤ 1 s (same machine) | M5 Pro + UE 5.8 |
| PT-7 | UE bridge push of 8129² → visible ≤ 5 s on the same machine; ≤ 8 s Mac → Windows over 1 GbE | M5 Pro + UE 5.8 |
| PT-8 | Cold start to interactive ≤ 3 s; open a Recipe ≤ 1 s | M1 8 GB |
| PT-9 | Node canvas 60 fps with 500 nodes and thumbnails | M1 8 GB |
| PT-10 | CUDA backend: PT-2 workload ≤ 0.3 s; PT-4 ≤ 40 s | RTX 4070 12 GB |
| PT-11 | Cross-backend parity: Metal vs CUDA heights max abs diff ≤ 0.05 m over a 3 km range at 2048², hillshade SSIM ≥ 0.99 | M5 Pro vs RTX 4070 |

PT-2 to PT-5 and PT-10 are **initial estimates**; M0 spike (a) confirms or revises them.

**Quality rules**
- The UI thread never waits on a cook, a build, disk or the UE bridge.
- Cook paths allocate nothing after warm-up; the HAL pools buffers.
- Every kernel has a CPU reference and a tolerance test.
- Byte-identical determinism holds per backend.
- `cargo deny` blocks GPL and non-commercial dependencies.
- No telemetry.
- Accessibility: colour-blind-safe wire palette, full keyboard navigation of the canvas, UI scaling, and reduced-motion mode (disables the wire pulse and reveal animation).

---

## 9. Milestones and acceptance criteria

### M0 — Foundations and spikes
- **Workspace and CI.** Cargo workspace with stub crates (§5.2). CI on macOS Apple Silicon (required). Non-blocking Linux/Windows build of `tir-hal` with the CUDA feature. `fmt`, `clippy -D warnings`, `cargo deny`.
- **Shell.** egui + wgpu window with an empty node canvas and a 3D viewport.
- **Spikes**, each a short report in `tir/docs/spikes/` with numbers:
  - (a) **Kernel bake-off:** Slang→MSL vs CubeCL→MSL vs hand MSL on three kernels (fBm + warp, one pipe-erosion step, MFD flow accumulation) at 1024², 4096² and 8192². Measures time, correctness against the CPU reference, Xcode GPU-capture debuggability and compile-time ergonomics. Run on M1 base and M5 Pro.
  - (b) The same Slang kernels → CUDA on an RTX 4070, with a parity check.
  - (c) **UE 5.8 bridge:** an external process pushes a 2017² float heightfield into a `LandscapeTexturePatch` on a named edit layer. Test every patch source mode (including the reported render-target bug) and the `FLandscapeEditDataInterface` fallback. Measure latency.
  - (d) Zero-copy: wgpu viewport rendering directly from a Metal 4 compute buffer (`wgpu-hal` interop) with no copy.
  - (e) Node canvas: reuse Atidarla's canvas vs `egui-snarl`; 500-node 60 fps test.
  - (f) Resolution independence: pipe erosion with age-based stepping at 512² vs 4096², measuring visual similarity at matched scale.
- **Acceptance:**
  - CI green.
  - All spike reports committed.
  - **T6 ratified or switched to CubeCL** based on (a)/(b).
  - T9 ratified or amended based on (c).
  - PT-2…PT-5 and PT-10 confirmed or revised.
  - Remaining Proposed decisions ratified or amended by the owner.

### M1 — Graph engine, core nodes, viewport, export
- **Engine.** `tir-core` cook engine (pull, dirty propagation, cancellation, caching); `tir-hal` Metal backend; undo/redo plus construction log; text project format with round-trip tests.
- **Editor and viewport.** Canvas FR-G1–G4, G6, G7; viewport FR-V1–V2.
- **Nodes.** Shapes, Noise, Combine and Shape-modifier nodes from §6 (≥25 nodes).
- **Export and import.** FR-I1 (height + sidecar) and FR-I3.
- **Acceptance:**
  - A 20-node graph built by hand saves and reloads identically, and its Git diff is human-readable.
  - PT-1, PT-8 and PT-9 are met.
  - An exported 2017² PNG16 imports into UE 5.8 by hand at the correct height, using the sidecar's Z scale.

### M2 — Simulation, masks, builds
- **Nodes and builds.**
  - Simulate family (Hydraulic, Thermal, Rain, Uplift & Erode, Age Erode, Fill Sinks, Amplify).
  - Masks, Colour/Splat and DEM Import nodes.
  - FR-S1–S5, FR-G5, FR-I2.
  - Tiled builds with halos; `tir build` and `tir bench`.
- **Learn foundations.** 5 Recipes; per-node "Show me" for ★ nodes.
- **Acceptance:**
  - PT-2…PT-5 are met.
  - A tiled 4×4 build matches a single-piece build of the same graph with no visible seams (max abs diff at tile borders ≤ 0.01 m).
  - Two runs give byte-identical output.
  - A Copernicus DEM import carries its attribution into the export sidecar.

### M3 — Unreal Engine 5.8 bridge
- **Bridge.** `TirBridge` plugin (Mac + Windows editor builds) and FR-U1–U10.
- **Fallbacks.** Python fallback script; printed import cheat-sheet.
- **Acceptance:**
  - A student-sculpted UE landscape is preserved: Tír pushes onto its own layer, and toggling that layer off restores the original exactly.
  - PT-6 and PT-7 are met.
  - Pull → erode → push round trip works.
  - Bake works.
  - A 4×4 World Partition tiled push aligns with no seams.
  - Mac → Windows LAN push works.
  - The Python fallback works on a machine with no compiled plugin.
  - Z-scale unit tests pass against Epic's worked example.

### M4 — Learn layer
- FR-L1–L11 (20 Recipes with lesson scripts), FR-V3, Glossary.
- **Usability study** with ≥8 first-year students who have never used a node tool.
- **Acceptance:**
  - In the study, ≥75 % of students, unaided, can:
    - (i) find which node makes a chosen feature of a Recipe (e.g. "the gullies") within 5 minutes;
    - (ii) modify a Recipe to meet a stated goal;
    - (iii) complete a Starter-level Wire-it-up.
  - Median time from first launch to the first terrain in UE is ≤ 10 minutes.
  - Findings are recorded in `tir/docs/milestones/M4.md`.
  - Every lesson passes `tir lesson-check`.

### M5 — CUDA backend and Windows
- `tir-hal` CUDA backend; Slang→PTX in the build; Vulkan/DX12 viewport via wgpu; Windows build of the app; installer.
- Parity test suite (PT-11) in CI, on a self-hosted NVIDIA runner.
- **Acceptance:**
  - Every Recipe builds on an RTX 4070 within PT-11 tolerance of the Mac result.
  - PT-10 is met.
  - The UE bridge works from Windows to Windows.

### M6 — Hardening, ML nodes, v1.0
- **Scope.** Node library completed to ≈60. ML nodes (FR-M1–M2). Signed + notarised DMG. User manual and docs-as-code: node pages generated from schemas. ≥40 examples.
- **Soak test.** A 2-hour session of continuous slider edits with no leak above 200 MB.
- **Accessibility pass.**
- **Acceptance:** all PT targets met; usability re-test confirms the M4 numbers; v1.0 tagged.

### M7+ — Horizon
- UE 5.8 Mesh Terrain target (FR-U11).
- Rivers and Snow at full quality.
- Glacial erosion.
- Sketch-to-Terrain.
- Unity, Godot and Houdini bridges.
- A typed MCP tool surface (agent-built graphs, matching World Creator 4's direction).
- An Atidarla Pack that embeds Tír graphs.
- A web viewer for lesson replays.
- UE 6 support.

---

## 10. Licensing and compliance

- **App:** MIT OR Apache-2.0 (T3); contributions under DCO.
- **UE plugin:** original source under the same licence on GitHub, with no copied engine code (snippets of 30 lines or fewer only). Prebuilt binaries are distributed via Fab. Get a legal review before the first public release (R§4.5).
- **Clean-room rule:** no code from GPL tools (Hesiod) or closed tools. Algorithms are implemented from papers. MIT-licensed references (Lague erosion, psrdnoise) may be adapted with attribution.
- **No model weights** in the repo or installers. DEM attributions are carried automatically into exports.
- **No competitor trademarks or node names** in the UI.

---

## 11. Success metrics (12 months after v1.0)

- Adopted in ≥2 taught modules (games/environment art). ≥200 students per year.
- Learning outcome: in a follow-up survey, ≥70 % of students say Tír helped them understand node-based workflows in other tools (UE Blueprints/Materials, Houdini, Blender Geometry Nodes).
- ≥300 GitHub stars, ≥15 external contributors, ≥50 community Recipes.
- No open data-loss bug in the UE bridge for two consecutive minor releases.

---

## 12. Open questions (resolve in M0)

1. Ratify T1–T15, especially T6 (Slang vs CubeCL) and T9 (patch vs edit-data) after the spikes.
2. Confirm the minimum Mac for students (M1 8 GB guard rail?) and whether labs run UE on Macs or on Windows PCs.
3. Should Tír live in its own repository? Recommended: yes, created once T1 and T2 are ratified. These docs sit in the Atidarla repo for now.
4. Reuse Atidarla's node canvas, or start on `egui-snarl` and converge later?
5. Name/trademark check for "Tír".
6. Which institution pilots the M4 usability study, and its ethics/consent process.
