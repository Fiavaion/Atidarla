# Tír (working title) — RESEARCH.md

**Version:** 0.1 · **Date:** 28 October 2026 · **Author:** Claude, for the project owner
**Status:** First research pass; feeds `tir/PRD.md` v0.1
**Subject:** an open-source, node-based heightmap and terrain generator in the class of QuadSpinner Gaea. It is built on Apple Silicon first, keeps a path to NVIDIA/CUDA, has a live bridge to the Unreal Engine 5.8 Landscape, and is designed to be a student's first node-based tool.
**Relationship to Atidarla:** this is a separate product. It may share the node canvas, the project-format conventions and the licence policy with Atidarla (see PRD decision T12), but none of Atidarla's decisions are changed by this document.

**Naming note.** The user brief says "Gaia". The QuadSpinner product is spelled **Gaea**. "Gaia" is a different product: a Unity terrain asset from Procedural Worlds **[U]**. This document uses "Gaea" throughout.

**Confidence legend.** Facts were gathered from vendor pages, release notes, docs, repositories and papers during this pass. Some domains were unreachable from the research environment: quadspinner.com, cgchannel.com, forums.unrealengine.com, 80.lv, apple.com/newsroom, arxiv.org and docs.rs. Facts from those sites come from search-result summaries and are marked **(S)**. Anything that could not be confirmed is marked **[U]**. Re-verify (S) and [U] items before quoting them externally. Prices change.

---

## 1. Executive summary

1. **Gaea is still Windows-only.** That is true of Gaea 2.3 (June 2026) and of the Gaea 3.0 Early Access (August 2026). QuadSpinner's knowledge base says "Currently, Gaea is Windows-only". Linux is under consideration with no timeline, and there is no Mac plan (S).
   - The Mac gap is **closing from other directions**:
     - World Machine Build 4065 (July 2026) ships a universal macOS binary.
     - JangaFX GeoGen 0.5 runs on M-series Pro, Max and Ultra chips.
     - World Creator 4, announced October 2026, targets macOS, adds nodes and ships an MCP server (S).
   - **"The only Mac node terrain tool" is therefore not a durable differentiator.** The durable ones are §1.2–§1.4.
2. **No terrain tool is designed to teach.** Every product relies on tutorials, YouTube and courses. Gaea's official learning hub is partly marked "being updated for 1.3", and there is no confirmed starter-template library.
   - The learning-science evidence points to concrete features:
     - Use-Modify-Create (Lytle et al. 2019).
     - PRIMM (Sentance et al. 2019).
     - Parsons problems (Ericson).
     - "Show the data" (Victor 2012).
     - Expertise reversal (Kalyuga et al. 2003).
   - No current terrain tool implements any of these. **A graph that can be replayed node by node** ("watch the graph build itself") is not offered by any mainstream DCC or terrain tool found (§6).
3. **Nobody has a true live link to Unreal.**
   - Gaea2Unreal (open source, modified MIT) and World Creator Bridge 2.0 both work as *export a file, then (re)import it*.
   - Only Houdini Engine cooks inside UE. It can write into a named landscape **edit layer**, which is the closest precedent for what we want.
   - UE 5.6–5.8 now make a better path possible (§4):
     - Edit layers are mandatory since 5.7.
     - Landscape Patches render into a dedicated procedural edit layer.
     - In 5.8, a patch can be bound to a landscape and an edit layer **by name** from Blueprint.
   - A non-destructive, sub-second live update into a named edit layer is achievable with a small C++ editor plugin.
4. **Apple Silicon is a good host for this workload.** Terrain simulation is bandwidth-bound stencil work over large 2D grids. On a bandwidth-bound stencil benchmark (FluidX3D), Apple GPUs reach 77–92 % of theoretical bandwidth, the same efficiency as NVIDIA cards (§5.4).
   - **Unified memory** is the decisive Mac advantage. M5 Max goes up to 128 GB at 614 GB/s; M5 Ultra up to 512 GB at about 1.2 TB/s (S).
   - That allows 16k² and larger builds **in one piece**, where a 32 GB consumer NVIDIA card has to tile.
   - It also allows zero-copy hand-off between simulation, viewport, file export and the UE bridge.
   - M5's per-core GPU **Neural Accelerators** (driven through Metal 4 tensor ops) suit the optional ML nodes.
5. **Cross-platform language choice.** Write the GPU kernels **once in Slang** (Khronos-hosted, Apache-2.0 with LLVM exception).
   - Compile them to **Metal Shading Language** on the Mac and to **CUDA** on NVIDIA. WGSL/SPIR-V remains as a fallback for other GPUs.
   - The host app is in **Rust**: `objc2-metal` for Metal 4, `cudarc` for CUDA, `wgpu` for the viewport, egui for the UI.
   - Slang's Metal target is still labelled *experimental*, so M0 includes a bake-off against **CubeCL** (Rust-native kernels, Metal + CUDA + wgpu) and hand-written MSL before the kernel language is ratified (§5).
   - Apple SIMD-groups and NVIDIA warps are both 32 wide, so wave-level tuning carries over between the two.
6. **Licensing is clean if we stay disciplined.**
   - Hesiod, the closest open-source analogue, is **GPL-3.0**: study it, never copy it.
   - Terrain Diffusion weights are MIT, and the TerrainSR super-resolution model is Apache-2.0.
   - DEM sources (Copernicus GLO-30, SRTM, USGS 3DEP) are free but carry attribution duties.
   - A UE editor plugin released as source under MIT/Apache follows the Cesium for Unreal precedent. Prebuilt editor binaries are a grey area under the UE EULA "Engine Tools" clause: ship source on GitHub, and binaries via Fab, and get a legal check (§4.5).

---

## 2. Market landscape

### 2.1 Comparison matrix (October 2026)

| Tool | Version | Platforms | Price | Paradigm | GPU | UE integration | Licence |
|---|---|---|---|---|---|---|---|
| **Gaea** (QuadSpinner) | 2.3 (15 Jun 2026); 3.0 Early Access (27 Aug 2026) | **Windows 10+ only** | Community free; Indie $99; Pro $199; Enterprise $299 (perpetual) | Node graph | Yes (CUDA 12.5+ / HIP 6.0+, CPU fallback) | Gaea2Unreal: file export, then import/re-sync | Proprietary; plugin modified MIT |
| **World Machine** | Build 4065 "Dragontail Peak" (Jul 2026) | Windows, **macOS (universal)**, Linux | Basic free (≤1025²); Indie $119 / Pro $299 / Studio $1,999 [U] | Node graph | [U] | Tiled RAW16/R16 export; no plugin found | Proprietary |
| **World Creator** | 2026.4 (Apr 2026); **v4 due Oct/Nov 2026** | Windows; v4 targets macOS + Linux | Community free (no export); Indie $99; Studio $499; rental $179/yr; Enterprise $1,499 | Layers + filters; v4 adds nodes and an MCP server | Yes | Bridge 2.0 for UE 5.7: sync folder; WP + PCG | Proprietary |
| **JangaFX GeoGen** | 0.5 (2025) | Windows, Linux, macOS 15 (M Pro/Max/Ultra only) | $9.99/mo or $149.99 indie | Real-time generator [U] | Yes | OBJ/FBX + maps ≤16K; no plugin [U] | Proprietary |
| **Instant Terra** | 2.5 [U date] | Windows | $149 / $199 [U] | Node graph | Yes [U] | Free UE plugin (UE4.26–UE5); an earlier funded UE4 **live link** plugin [U status] | Proprietary |
| **Houdini heightfields** | Houdini 22 (Jul 2026) | Windows, macOS (Apple Silicon), Linux | Apprentice free; Education ~$95/yr; Indie ~$299/yr [U] | SOP node graph; H22 adds Copernicus (GPU) heightfields | Yes (Copernicus) | Houdini Engine cooks the Landscape inside UE, edit-layer aware | Proprietary; HE4U source public |
| **UE built-in** | 5.8 | Windows, macOS, Linux | Free (UE EULA) | Sculpt + Landmass Blueprint brushes + PCG; Mesh Terrain (Experimental) | Yes | Native | UE EULA |
| **Blender** | A.N.T. Landscape (bundled); Terrain Mixer 3.1.2 | Win/Mac/Linux | Free / paid add-ons | Geometry Nodes / operator panels | Cycles/EEVEE | Manual export | GPL |
| **Hesiod** | WIP (last push May 2026) | Windows, Linux | Free | Node graph (Qt/C++) | [U] | Heightmap export | **GPL-3.0** |
| **TerraForge3D** | last update Nov 2025 | Windows (Mac [U]) | Free | Node-based [U] | [U] | Export | MIT |
| **TerreSculptor** | 2.0 (c. 2020) | Windows | Free | Layer/tool-based | [U] | Export | Freeware |
| **Terrain3D (Godot)** | 1.0.2 (May 2026) | All | Free | Terrain runtime/editor, not a generator | Yes | n/a | MIT |

### 2.2 Gaea in detail (the reference product)

- **Releases:**
  - 2.0: 1 Aug 2024. Graph redesigned "to reduce the learning curve", with node modifiers for lighter graphs.
  - 2.1.1: Variables, Compute, Expressions.
  - 2.2: 15 Jul 2025. Selective precipitation in GPU Erosion_2, "Frosty" nodes, Gaea2Unreal for UE 5.6.
  - 2.3: 15 Jun 2026. Macros (a graph packaged as one node), baking up to 16K, Lake "Simple" mode, better Regions.
  - 3.0: announced Dec 2025. Early Access 27 Aug 2026 to pre-order customers; new UI and viewport, Dunes, vector tools for roads and rivers, sand and snow simulation, USD/OCIO (Pro+), an SDK (C# / C++/CLI, .NET 6+). (S)
- **Tiers and caps:** Community exports ≤1024² (creates and views up to 4096²); Indie 8192²; Pro/Enterprise 16,384² single build and 262,144² tiled (S, tier mapping [U]). Revenue caps: Indie under $100k, Pro up to $1M.
- **Education:** a free Student + Teacher licence (16+, accredited institution, related field, 1 year, renewable). If more than 10 % of a class uses free licences, the school must buy an annual **Classroom Licence**. No commercial use.
- **Build features:** tiled builds (nodes that cannot tile are flagged; a TileGate node handles them); **Build Swarm** command-line builds with variable overrides.
- **Engine architecture (S):** the engine is a separate layer from the UI and handles tiling and SPMD execution. Minimum GPU is a GTX 1080 / RX Vega 64.
- **Gaea2Unreal:**
  - Built on Gaea's Unreal export node. Imports the heightmap plus weightmaps, with re-import/sync; supports World Partition.
  - Branches for UE 5.3–5.6. **No 5.7 or 5.8 branch found [U].**
  - Known slow World Partition import on 5.5.
  - **Not a live link.**
- **Onboarding:** a docs Learning hub ("Your First Terrain", 50+ QuickStarts). Much beginner material is third-party. There is **no confirmed in-app learning mode or simple mode [U]**.

### 2.3 Competitor notes that shape the PRD

- **World Creator's history is a warning about node overload.** It stayed layer-based for years because testers "got lost among 80 and more nodes"; v4 now adds nodes (S). **Implication:** beginners must start with small graphs, and the layer-stack mental model needs a bridge to the graph (PRD FR-L3 "Stack ⇄ Graph").
- **World Machine admits erosion is resolution-dependent.** Its LTE builds patch over this by downsampling the high-res build to replace the preview [U]. **Implication:** express parameters in world units and continuous time (§3.4).
- **World Creator 4 ships an MCP server.** Agent-driven terrain authoring is arriving. **Implication:** our typed tool surface can come later (PRD horizon), but the project format should be agent-friendly from day one: text, stable IDs, typed schemas.
- **Houdini Engine is the gold standard for UE integration.** Its relevant settings:
  - `unreal_landscape_output_mode=1` modifies an edit layer on an existing landscape;
  - `unreal_landscape_editlayer_name` / `_after` place that layer in the stack;
  - a temporary cook layer is replaced by a baked layer.
  We should match that behaviour without requiring Houdini.

### 2.4 Forum pain points (what users complain about)

1. **No Gaea for Mac.** Users are pointed to World Creator or Terragen.
2. **Terracing.** It comes from 8-bit data, or from stretching a 16-bit range too far. 16-bit gives 65,536 levels, so precision drops as the height range grows.
3. **UE Z-scale maths.** Users find `Z = range_m × 100 / 512` confusing, and mismatched scales are the #1 round-trip error.
4. **Bad input values.** Non-zero base elevation and NODATA spikes from DEMs.
5. **Re-import problems.**
   - Weightmaps do not re-import on tiled worlds.
   - Swapped tiles lose their paint layers.
   - World Partition with Virtual Texturing crashes above 16k.
   - Students must memorise UE's odd valid sizes (2017, 4033, 8129).
6. **"Unreal can't generate heightmaps / has no erosion"** is the reason external tools exist at all.

**Evidence gap.** No study measuring how novices learn node-based terrain tools was found. Our own usability tests will be the evidence (PRD M4).

---

## 3. Algorithms (what the node library is built on)

### 3.1 Noise and primitives (all embarrassingly parallel; GPU-trivial)

| Technique | Reference | Note |
|---|---|---|
| fBm, ridged / hybrid / hetero multifractal | Musgrave in *Texturing & Modeling* (Ebert et al., 3rd ed. 2002) | Offset and gain matter for the multifractal variants |
| Domain warping `f(p + h(p))` | Quilez, iquilezles.org/articles/warp | Produces natural land bridges and valleys |
| Worley / cellular | Worley, SIGGRAPH 1996, DOI 10.1145/237170.237267 | Craters, mesas, cells |
| Simplex / periodic simplex (psrdnoise) | Gustavson & McEwan, JCGT 11(1) 2022 | Analytic derivatives and **tileable**; code MIT per headers [U]. Perlin's simplex patent US 6,867,776 expired Jan 2022 |

### 3.2 Erosion and hydrology

| Process | Key reference | GPU suitability | Node in PRD |
|---|---|---|---|
| Grid hydraulic ("virtual pipes" shallow water) | Mei, Decaudin & Hu, Pacific Graphics 2007, DOI 10.1109/PG.2007.15 | Excellent: gather-only, deterministic | Hydraulic Erode |
| Interactive multi-layer hydraulic + bank slippage | Šťava et al., SCA 2008 | ≥20 fps at 2048×1024 on 2008 GPUs; independent GPU tiles | Hydraulic Erode (layers) |
| Particle / droplet erosion | Beyer thesis 2015 [U]; Lague *Hydraulic-Erosion* (MIT) | Races make it **non-deterministic** unless heights use fixed-point i32 `atomicAdd` or steps are split across dispatches | Rain Erode (deterministic mode) |
| Thermal / talus | Musgrave, Kolb & Mace, SIGGRAPH 1989 | Excellent | Thermal Erode |
| Stream power + tectonic uplift | Cordonnier et al., CGF 2016, DOI 10.1111/cgf.12820 | Graph-based; medium | Uplift & Erode |
| Interactive stream-power authoring | Schott et al., TOG 42(5) SIGGRAPH 2023, DOI 10.1145/3592787 | Fast drainage-area approximation; the user paints uplift; code on GitHub [U licence] | Uplift & Erode |
| **Analytical erosion, time as a continuous slider** | Tzathas et al., CGF 43(2) EG 2024 | Multigrid; **resolution-independent** | Erosion "Age" parameter |
| Multi-scale amplification | Schott et al., TOG 43(4) SIGGRAPH 2024, DOI 10.1145/3658200 | Keeps drainage consistent across scales | Amplify |
| Flow routing D8 / MFD / D∞ | O'Callaghan & Mark 1984; Freeman 1991; Tarboton 1997 | MFD/D∞ give smoother masks | Flow, Wetness masks |
| Pit filling | Priority-Flood, Barnes et al., C&G 2014 | CPU reference; GPU via FastFlow | Fill Sinks |
| GPU flow + depression routing | **FastFlow**, Jain et al., CGF 43(7) 2024 (paper CC BY 4.0) | O(log n) flow; 5× / 34–52× faster at 1024² | Flow (GPU) |
| Rivers from hydrology | Génevaux et al., TOG 32(4) 2013 | Graph + carving | River (v1.x) |
| Snow + avalanches | Cordonnier et al., CGF 37(2) 2018 | Real time over 10×10 km | Snow (v1.x) |
| Glacial erosion | Cordonnier et al., TOG 42(4) 2023 | Learned ice flow; heavier | Horizon |
| Debris flow, Unerosion, Arenite | Jain 2024; Yang 2024; Yang 2025 | Research-grade | Horizon |

### 3.3 Machine-learning terrain (optional nodes, model manager, never bundled)

| Model | What it does | Weights / licence | Note |
|---|---|---|---|
| **Terrain Diffusion** (Goslin, arXiv 2512.08309, Dec 2025) | Infinite, seed-consistent, constant-time random access ("successor to Perlin") | `xandergos/terrain-diffusion-30m` / `-90m` on Hugging Face, **MIT**, plus an ONNX export | Trained on Copernicus GLO-30, so attribution likely carries over [U] |
| MESA (CVPR 2025 W) | Text to terrain (elevation + imagery) | `NewtNewt/MESA`, licence "other" [U] | SD 2.1 lineage: treat as restricted |
| Lochner et al., CGF 2023 | Sketch to terrain with diffusion; super-resolution | Code + models released [U licence] | Good fit for a "Sketch" node |
| **TerrainSR** | DEM super-resolution 100 m → 10 m | `joe-gibbs/terrainsr`, **Apache-2.0**, ONNX | 0.74 s per 50×50 km patch on an RTX 4070 |

### 3.4 Determinism, tiling and resolution independence

- **Halo / apron.** Erode each tile with a border at least as wide as the erosion reach, then crop. World Machine users add about 25 % margin.
- **Seeding.** Seed every random stream by **world coordinate**, never by tile index or thread ID.
- **No float atomics on the hot path.** Use gather-style updates (pipe model) or fixed-point i32 atomics. This matters doubly because Apple GPUs have **no FP64** (§5.1).
- **Preview ≠ build.** Erosion is resolution-dependent. The mitigations are:
  1. parameters in **metres and years**, not pixels and iterations;
  2. continuous-time or analytical solvers where they exist (Tzathas 2024);
  3. multi-scale amplification (Schott 2024);
  4. a per-node "preview fidelity" warning when a node is known to diverge between preview and build resolution.

### 3.5 Real-world elevation data (DEM import node)

| Source | Licence | Duty |
|---|---|---|
| Copernicus GLO-30 | Free worldwide (Armenia/Azerbaijan excluded) | Mandatory © DLR/Airbus notice; "produced using Copernicus WorldDEM-30" for modified data; commercial terms [U] |
| SRTM GL1 | Generally public domain (NASA/NGA/USGS); one catalogue says research/education only [U conflict] | Citation requested |
| USGS 3DEP | Public domain | Credit requested |
| OpenTopography | Portal, not a licensor | Inherits the source licence |

The DEM node must write attribution into the project and into export sidecars automatically.

---

## 4. Unreal Engine 5.8 Landscape — integration research

### 4.1 Status

- **UE 5.8 shipped on 17 June 2026** at Unreal Fest. Other reports say 19 or 23 June [U].
- Epic calls it the last planned major UE5 release; UE6 Early Access is targeted for late 2027.
- **macOS editor requirements:**
  - macOS Sonoma 14.5+;
  - Xcode 26.0+ (26.4 incompatible);
  - Apple Silicon M1/M2 minimum, **M3 recommended**;
  - Nanite and Virtual Shadow Maps on M2+ (Beta).
  - The page is not explicitly labelled 5.8 [U].
- **Implication: students can run both our app and UE on the same Mac.** The bridge must also work across a LAN, from a Mac to a Windows UE workstation, because many labs run UE on Windows.

### 4.2 Landscape changes 5.6 → 5.8 that matter to us

| Version | Change | Consequence for the bridge |
|---|---|---|
| 5.6 (Jun 2025) | Non-edit-layer landscape path **removed**; PCG GPU sampling of landscape RVT | Everything is edit-layer based |
| 5.7 (Nov 2025) | "Enable Edit Layers" checkbox gone; PCG production-ready | We can assume edit layers exist |
| 5.8 (Jun 2026) | Edit layers exposed to Blueprint, with names; **`LandscapePatchComponent` binds landscape + edit layer by name**; `landscape.ForceLayersUpdate` → `ForceLayersFullUpdate`; brush optimisation | Target a layer by a stable name (e.g. "Tír"), not a GUID |
| 5.8 | **Mesh Terrain (Experimental)**: mesh-based, overhangs and caves, Nanite; "Import Heightmap" creation mode; no Landscape → Mesh Terrain conversion (use a TextureModifier) | Horizon target; users report imports failing around 24k [U] |
| 5.8 | Open forum bug: LandscapeTexturePatch in "Texture Backed Render Target" mode misapplies captured height [U] | M0 spike must test the patch source modes |

**Edit-layer architecture.**
- `ULandscapeEditLayerBase` has two branches:
  - **persistent**: `ULandscapeEditLayer`, Splines;
  - **procedural**: `ULandscapeEditLayerProcedural`, which includes `ULandscapePatchEditLayer`.
- Patches reference their layer and order themselves by priority.
- Layers merge on the GPU through render targets (`FMergeRenderContext`).
- The default limit is 8 layers (configurable).

### 4.3 Import constraints the app must encode

- **Vertices per side** = components × (sections per component × quads per section) + 1.
  - Quads per section ∈ {7, 15, 31, 63, 127, 255}; sections per component ∈ {1, 2×2}.
  - Epic recommends ≤ 1024 components (32×32).
- **Common sizes:** 505, 1009, **2017**, **4033**, **8129** (2×2 sections) and 8161 (1 section).
- **World Partition** suggests valid sizes and pads or stretches otherwise. A forum formula gives (511·N) − (N − 1) [U].
- **Formats:** 16-bit greyscale PNG and RAW `.r16`. Custom formats are possible in C++ via `ILandscapeHeightmapFileFormat`.
- **Z scale:**
  - At Z = 1 the raw 16-bit range spans −256 … +255.992 units. Raw value 32768 is zero height.
  - The default **Z = 100 gives ±256 m**.
  - **Z = range_m × 100 / 512.** Epic's example: a 4207 m range gives Z ≈ 821.68.
  - At Z = 100, one metre is 128 raw steps.
- **XY scale:** default 100 means 1 m per vertex [U, not directly sourced].
- **Tiled World Partition import:**
  - name tiles `name_x0_y0`, `name_x1_y0`, …;
  - use Import Type "Subregion" for changes to an existing landscape;
  - use "Flip Y Axis" if tiles are misaligned.
  - A 5.7.1 bug repeats tiles [U].
- **Weightmaps:** one greyscale image per paint layer.

**Product implication.** The app should *never* ask a student for a Z scale. It works in metres and computes the Z scale, XY scale, valid resolution and tile naming itself, or the bridge does. That alone removes the #1 forum pain point (§2.4).

### 4.4 Integration paths compared

| Path | Latency | Non-destructive | Edit-layer aware | Complexity | Verdict |
|---|---|---|---|---|---|
| (a) File export + manual or one-click reimport | seconds–minutes | partial (Subregion into the chosen layer) | partial | low | **Always available** as the baseline and fallback |
| (b) Editor Python (`unreal` module): `LandscapeProxy.landscape_import_heightmap_from_render_target(…, edit layer index)`; `LandscapeTexturePatch` is scriptable | ~1–5 s [U] | yes | yes | low–medium | **Zero-install fallback**: ships as a Python script, no compiled plugin needed |
| (c) Remote Control API (HTTP :30010 / WS :30020, `PUT /remote/object/call`) | ~100 ms per call + payload | inherits | inherits | low | **Control channel only**: JSON payloads (base64) are poor for 4k–8k maps; WS text limit 1 MB [U] |
| (d) Custom C++ editor plugin: `FLandscapeEditDataInterface(…, layer GUID)::SetHeightData`, `FScopedSetLandscapeEditingLayer` | sub-second + GPU merge | yes | yes | high; per-engine-version builds | Fallback inside our plugin |
| (e) **Landscape Patch / Texture Patch** fed by data pushed from the plugin (internal texture, texture asset or render target; additive or alpha blend; native / world-units / 0–1 + offset encodings; weight patches per paint layer) | **fastest**: one GPU re-merge | **yes**: dedicated procedural layer, editor-only data | **yes**: by name in 5.8 | medium | **Primary live path** (Experimental plugin; check the 5.8 RT bug) |
| (f) Live Link | n/a | n/a | n/a | n/a | Not suitable: built for transform and animation streams |
| (g) Mesh Terrain TextureModifier (5.8 Exp.) | [U] | yes | n/a | medium | Horizon spike |

**Recommended architecture** (detailed in PRD §7.5):
- The **app** streams height and weight tiles (16-bit or float32) plus a metadata header (metres, origin, resolution, layer name) over TCP. On the same machine it uses a shared-memory fast path.
- A small **C++ editor plugin**, "Bridge", receives them and writes into a `LandscapeTexturePatch`, or into a named edit layer via (d) when patches misbehave.
- Optionally the plugin can **bake** the patch into a persistent layer, mirroring Houdini Engine's "temporary cook layer → baked layer".
- The plugin also serves the reverse direction: reading an existing landscape back into the app as an input node.
- Path (b) ships alongside as a script for locked-down lab machines where compiled plugins cannot be installed.

### 4.5 Licensing of the UE plugin

- **What the EULA says.**
  - Plugin code that relies on the engine is subject to the UE EULA.
  - "Engine Tools" (Editor/Developer code) may be publicly distributed only via Fab/Marketplace or a fork in Epic's GitHub network.
  - Snippets of up to 30 lines may be posted publicly.
- **Precedent.**
  - Cesium for Unreal is Apache-2.0 on GitHub and also on Fab.
  - Gaea2Unreal and Houdini Engine for Unreal publish their source publicly.
- **Interpretation [U], not legal advice.**
  - Original plugin source (no copied engine code) under MIT OR Apache-2.0 on GitHub matches established practice.
  - Prebuilt editor binaries should go through Fab.
  - Get a legal check before release.

---

## 5. Platform and technology: Apple-first, CUDA-ready

### 5.1 Apple Silicon and Metal in late 2026

| Chip | Announced | GPU cores | Max memory | Bandwidth |
|---|---|---|---|---|
| M5 | Oct 2025 | 10, each with a Neural Accelerator | 32 GB | 153 GB/s |
| M5 Pro | Mar 2026 | up to 20 | 64 GB | ~307 GB/s |
| M5 Max | Mar 2026 | up to 40 | 128 GB | 614 GB/s |
| M5 Ultra | Aug 2026 | up to 80 | 512 GB [U ship date] | ~1.2 TB/s |
| M6 (base) | Aug 2026 | 12 | n/a | 170 GB/s |

(S) for all rows. For comparison, the M4 Max has 546 GB/s and up to 128 GB.

**Metal facts that shape the design.** Sources: Metal Feature Set Tables (May 2026) and the MSL 4.1 spec.
- **Families:** M1 = Apple7, M2 = Apple8, M3/M4 = Apple9, M5 = Apple10. **The Metal 4 API needs Apple7+**, so every Apple Silicon Mac is covered.
- **Max 2D texture:** **16,384 px on M1–M4, 32,768 px on M5.** A 16k² map plus a halo does not fit in one texture before M5. **Canonical storage is therefore buffers**; textures are only views for the viewport.
- **No FP64** in MSL ("Metal does not support the double … data types"). 64-bit integers are supported.
  - Kernels are designed f32-first, with fixed-point i32/i64 for deterministic accumulation.
  - CUDA has FP64, but we must not depend on it.
- **Atomics:** float atomics on Apple7+; full 64-bit atomics only on Apple9+.
- **Threadgroups:** 1,024 threads and 32 KB threadgroup memory. SIMD-group reductions and matrix ops on Apple7+.
- **SIMD-group width is 32, the same as an NVIDIA warp.**
- **Unified memory:** `MTLStorageMode.shared` is CPU/GPU zero-copy.
  - `maxBufferLength` is only guaranteed to be ≥ 256 MB and must be queried.
  - `recommendedMaxWorkingSetSize` is about 75 % of RAM [U].
  - One 16k² f32 layer is 1 GiB, so an erosion state of 8–12 layers needs 8–12 GiB: comfortable on 64–128 GB Macs.
- **Metal 4 (WWDC25):**
  - `MTL4CommandQueue`, with command buffers decoupled from queues and encodable in parallel;
  - command allocators;
  - a unified compute encoder;
  - **argument tables** (bindless);
  - **residency sets**, now the only residency mechanism;
  - an explicit compiler;
  - an **ML command encoder** that runs Core ML packages inside a command buffer;
  - **tensors in the API and in MSL**, with Metal Performance Primitives TensorOps inlined into shaders.
- **WWDC26 (macOS 27):**
  - quantized tensor types (4/8-bit float, 2-bit int, MX scaling);
  - cooperative tensors feeding matmul directly;
  - TensorOps use the **M5 Neural Accelerators automatically** and fall back on older GPUs;
  - small MLPs can be trained online per frame.

**Where the Mac can be specifically optimised** (each becomes a PRD requirement):
1. **Zero-copy pipeline.** Simulation output, viewport mesh, file writer and UE bridge all read the same shared-storage buffer. There are no uploads and no readbacks.
2. **Big single builds.** 16k² graphs build without tiling on 64 GB+ Macs, and larger on M5 Max/Ultra. Tiling remains for limits beyond memory and for parity with CUDA.
3. **Low-overhead graph dispatch.** A graph cook issues many small dispatches. Metal 4 argument tables, residency sets and parallel command encoding keep the CPU cost per node low.
4. **Neural Accelerators for ML nodes.** Run Terrain Diffusion and super-resolution through Core ML / Metal tensors (and the MTL4 ML encoder) inside the same command stream as the simulation.
5. **Energy-aware preview.** On battery, cap preview resolution and dispatch rate; schedule CPU-side cooks with macOS QoS classes on performance cores.
6. **Profiling.** Instruments / Metal System Trace integration, with `os_signpost` per node cook.

### 5.2 Kernel languages that reach both Metal and CUDA

| Option | Metal | CUDA | Other | Maturity (Oct 2026) | Licence | Fit |
|---|---|---|---|---|---|---|
| **Slang** | Yes, README says **experimental** (compute supported) | **Yes** (CUDA C++/PTX via NVRTC, compute) | SPIR-V, HLSL, WGSL (exp.), CPU | v2026.20 (17 Oct 2026), releases every 1–2 weeks; Khronos-hosted since Nov 2024 | Apache-2.0 WITH LLVM-exception | **Best single-source fit**; modules, generics, autodiff (useful for inverse/"Unerosion"-style tools) |
| **CubeCL** (Rust `#[cube]`) | Yes (MSL via wgpu or "metal-native") | Yes (LLVM → PTX) | HIP, SPIR-V, WGSL, CPU | 0.11.3, pre-1.0 | MIT/Apache-2.0 | **Strong alternative**: kernels in the host language; Burn ML on the same backends |
| slang-rhi | Yes (compute; no timestamps or shared handles) | Yes (compute) | D3D, Vulkan, WebGPU | no tagged releases; unstable API | Apache-2.0 + LLVM | C++ host only; not adopted |
| wgpu / WGSL | Yes | **No** (Vulkan on NVIDIA instead) | DX12, Vulkan, GL | v30.0.1 | MIT/Apache-2.0 | Viewport; portable fallback compute; no Metal 4 tensors; f64 only on Vulkan |
| MLX | Native | Yes (Linux; Windows CI in 0.32.x) | CPU | v0.32.4 | MIT | Array/ML framework; custom kernels are per-backend (`metal_kernel` / `cuda_kernel`) |
| Mojo / MAX | Initial (M1–M5) | Yes | AMD | Mojo 1.2; Apple path still being tuned (S) | Apache-2.0 + LLVM (parts under MAX licence) | Watch; too young on Apple |
| Halide | Yes | Yes | Vulkan, WebGPU (beta) | v22.0.0 | MIT | Good for stencils, awkward for iterative sims |
| Taichi | Yes | Yes | Vulkan | **dormant** since Jul 2025 | Apache-2.0 | Avoid |
| Triton, Kokkos | No / third-party alpha | Yes | — | — | — | No Apple path |
| AdaptiveCpp (SYCL) | Experimental | Yes | HIP, L0 | early on Metal | BSD-2 [U] | Too early |
| OpenCL | Deprecated on macOS since 2018 | Yes | — | legacy | — | Do not build on it |
| Swift + MSL only | Native, full Metal 4 | **No** | — | — | — | Locks the app to macOS; rejected |

**Recommendation (PRD decisions T5 and T6).**
- **Kernels:** a single source in **Slang**, compiled to MSL (Metal) and CUDA, with WGSL as fallback.
- **Host:** **Rust**, with a thin compute HAL that has three backends:
  - `metal`: `objc2-metal` 0.3.2, which covers the Metal 4 traits;
  - `cuda`: `cudarc` 0.19;
  - `wgpu`: Vulkan/DX12 fallback.
- **Escape hatch:** hand-written MSL / CUDA for kernels where a platform feature matters, such as Metal tensors for ML nodes. Every such kernel must have a portable Slang reference version and a parity test.
- **Gate:** an M0 bake-off on three kernels (fBm + warp, one pipe-erosion step, flow accumulation) comparing Slang→MSL, CubeCL→MSL and hand MSL on speed, correctness and debuggability. If Slang's Metal target fails the gate, CubeCL becomes the kernel language. That switch is cheap because CubeCL also targets CUDA.

**Why Rust and not C++ or Swift.**
- Swift locks out CUDA, which the brief rules out.
- C++ (Dear ImGui + imgui-node-editor + metal-cpp + slang-rhi) is viable, but slang-rhi has no stable API.
- Rust gives memory safety in a long-running interactive app, `cargo`-based CI, and reuse of Atidarla's egui node canvas and project-format conventions.
- The UE plugin is C++ regardless, but it is small.

### 5.3 Host UI and viewport

- **UI:** egui 0.37 with a node canvas.
  - Either reuse Atidarla's planned custom canvas or start from `egui-snarl` 0.12; that choice is an M0 spike.
  - `egui_node_graph2` is stale.
- **Viewport:** `wgpu` (native Metal backend on macOS), rendering a GPU-displaced grid, hillshade, slope/flow overlays and water.
  - On macOS it can share the compute device's `MTLBuffer` through `wgpu-hal`.
  - On CUDA it uses Vulkan external-memory interop (cf. Atidarla R§4.8 `wgpu-cuda-interop`) or a staging copy.

### 5.4 Performance evidence

**No published M-series GPU erosion benchmark exists.** The best bandwidth-bound proxy is FluidX3D, a lattice-Boltzmann stencil code (MLUPs/s, FP32 arithmetic with FP16 storage):

| GPU | Bandwidth (GB/s) | MLUPs/s | Efficiency |
|---|---|---|---|
| RTX 5090 | 1,792 | 18,459 | 79 % |
| RTX 4090 | 1,008 | 11,091 | 85 % |
| M2 Ultra | 800 | 8,769 | 84 % |
| M3 Ultra | 819 | 8,174 | 77 % |
| M2 Max | 400 | 4,641 | 89 % |
| M5 (10-core) | 153 | 1,596 | 80 % |

- **Inference, not measured.** Stencil throughput scales with bandwidth. M5 Max ≈ 0.34× an RTX 5090, and M5 Ultra ≈ 0.67× with 16× the memory.
- **Base M-series chips** (M1–M5, 100–153 GB/s) are the classroom reality. Preview budgets must be set for them (PRD §8).
- **Old data point:** a 1024² GPU erosion step ran at ≤ 2 ms on a GTX 1050.
- **M0 must produce our own numbers.**

---

## 6. Learning design: a first node-based tool

### 6.1 Evidence

| Finding | Source | Design consequence |
|---|---|---|
| Use → Modify → Create progressions lower perceived difficulty for novices | Lytle et al., ITiCSE 2019 | Presets ("Recipes") are the *use* stage; guided tweaks the *modify* stage; blank canvas the *create* stage |
| Predict → Run → Investigate → Modify → Make | Sentance, Waite & Kallia, SIGCSE 2019 | "Predict" cards on presets: *what happens if you bypass this node?* Guess, then see |
| Parsons problems are as effective as writing code, and faster | Ericson et al. [U 2017]; Hou, Ericson & Wang, ITiCSE 2023 | "Wire it up" challenges: nodes are placed, wires are missing |
| Worked examples help novices but hurt experts (expertise reversal) | Kalyuga et al., *Ed. Psych.* 2003 | Scaffolds fade and are hideable; Learn mode is never forced |
| Make the notional machine explicit | du Boulay 1986; Sorva, TOCE 2013 | Show that data flows along wires and each node is a function of heightfields: cook-order numbers, flow animation, value probe |
| "Show the data"; follow the flow; create by reacting | Victor, *Learnable Programming* 2012 | Live thumbnail on every node, on by default; instant recook |
| Visual languages: good closeness of mapping, but viscosity and tangle hurt working memory | Green & Petre, JVLC 1996 | Small beginner graphs, groups/macros, auto-layout, wire colours by type |
| Blocks help learning; text feels "real" | Weintrop & Wilensky, TOCE 2017 | Pair the graph with a readable "what this does" sentence per node and a text project file |
| Low floor, high ceiling, wide walls | Resnick et al., CACM 2009 | Recipes span deserts → alpine → volcanic → river valleys → real-world DEM |
| Node tools: novices understand components but not chaining | eCAADe 2025; SIGraDi 2011 (Grasshopper) | Teach chaining first: the Stack view (a chain) leads into the Graph view (branches) |
| Testers "got lost among 80+ nodes" | World Creator dev history (S) | Beginner palette of about 15 nodes; full palette unlocked by choice |

### 6.2 Product precedents

- **Per-node preview and solo:** Blender Viewer node and node timings overlay; Gaea pin and underlay; TouchDesigner viewers.
- **Inspect and trace:** Unreal Blueprint Debugger (watches, breakpoints, execution trace with timing, hover values).
- **Examples attached to every node:** TouchDesigner OP Snippets (1000+ live examples, right-click any operator). This is the best precedent for "every node ships a runnable mini-example".
- **Promoted parameters ("GUI over a hidden graph"):** Gaea Variables / Exposed Properties; Substance exposed parameters (SBSAR sliders); Houdini HDA interfaces; Blender group inputs in the modifier panel.
- **Graph replay:** no terrain or DCC product found. The nearest are the Drupal Modeler execution replay (step, auto-play at 0.5–2×, active node highlighted), Valve's AnimGraph timeline scrubber and Stata's `graph play`.
  - **A construction log stored in the project, replayable node by node with captions, is an open differentiator.**
- **Structured curricula:** SideFX *Houdini Foundations* (ends in a terrain chapter); learn.derivative.ca exercises; Gaea "Your First Terrain".

### 6.3 Synthesis: the "reverse-engineer a preset" loop

1. **Open a Recipe.** It opens in **Simple view**: a short slider panel of promoted parameters in plain language (e.g. "Mountain height (m)", "Rain (years)") with a large 3D viewport. The experience is GUI-like.
2. **"How is this made?"** The slider panel slides aside to reveal the graph. Every slider is visibly wired to the node parameter it drives; highlighting a slider highlights its node.
3. **Replay.** Scrub the construction log, so the graph assembles node by node with captions and per-node thumbnails.
4. **Predict.** Cards ask *"What will bypassing Thermal Erode do?"*. The student picks an answer, then bypasses the node and sees the result.
5. **Modify.** Guided tasks change parameters, swap a node, or add a mask branch.
6. **Wire it up (Parsons).** The same Recipe with wires removed; the student reconnects it.
7. **Create.** A blank canvas, with the beginner palette and the Stack view available.

---

## 7. Market gaps and proposed differentiators

| # | Differentiator | Evidence | Effort |
|---|---|---|---|
| **X1** | **Learn-first UX**: Recipes, Simple ⇄ Graph reveal, Replay, Predict cards, Parsons challenges, per-node mini-examples | §6; no competitor has an in-app learning mode | M–L |
| **X2** | **Live, non-destructive UE 5.8 bridge** into a named edit layer, metres-native (no Z-scale maths), bidirectional | §4; all competitors use files | M |
| **X3** | **Apple-Silicon-native engine**: unified-memory single builds, zero-copy, Metal 4 dispatch, Neural Accelerator ML nodes | §5.1 | M |
| **X4** | **Free and open (MIT OR Apache-2.0), no resolution caps, no classroom licence** | Gaea Community caps export at 1024² and needs a Classroom Licence if more than 10 % of a class uses free licences; World Machine Basic is capped at 1025² | S |
| **X5** | **Single-source kernels → CUDA**: the same graph builds on NVIDIA with parity tests | §5.2 | M |
| **X6** | **World-unit, time-based parameters** for preview/build consistency | §3.4; World Machine admits resolution dependence | M |
| **X7** | **Text project format + construction log**: Git-friendly assignments, diffable, agent-ready | Atidarla A11 precedent; World Creator 4 MCP | S |

**Not differentiators** (do not over-invest):
- "First on Mac": World Machine and GeoGen are already there, and World Creator 4 is coming.
- Raw erosion quality versus Gaea: aim for parity on the classic looks, not superiority, in v1.0.

---

## 8. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Slang Metal target remains experimental or produces slow MSL | Medium | M0 bake-off; CubeCL fallback; hand-MSL escape hatch with parity tests |
| UE Landscape Patch plugin is Experimental and has a 5.8 render-target bug | Medium | Use internal-texture patch mode; `FLandscapeEditDataInterface` fallback; Python-script fallback; pin to a 5.8 hotfix |
| Per-engine-version plugin rebuilds (5.7, 5.8, later 6.x EA) | High | Thin plugin (<3k LoC), CI builds per UE version, source distribution |
| Erosion looks differ between preview and build | High | World units, analytical/time-based solvers, fidelity warnings, "build preview" button |
| Cross-backend numeric drift (Metal f32 vs CUDA) | Medium | Tolerance-based golden tests (heights within ε, SSIM of hillshade), deterministic fixed-point accumulation |
| World Creator 4 lands on Mac with nodes first | Medium | Compete on X1, X2, X4 (learning, live UE, free/open), not on "Mac" |
| Students overwhelmed by node count | Medium | Beginner palette, Stack view, small Recipes, usability tests in M4 |
| GPL contamination (Hesiod) or non-commercial models | Low | `cargo deny`, clean-room policy, model manager with licence badges |
| 16k² texture limit on M1–M4 | Certain | Buffers as canonical storage; viewport LOD |

---

## 9. Open questions to verify (M0)

1. Our own erosion numbers on M1 base, M3/M4 base, M5 Pro/Max and RTX 4070 (fBm, pipe step, flow accumulation).
2. Slang 2026.2x MSL output quality and debuggability (Xcode GPU capture) versus CubeCL and hand MSL.
3. LandscapeTexturePatch source modes in UE 5.8: which mode accepts a programmatic 2017² / 4033² float update fastest without the reported bug?
4. Whether Gaea2Unreal has a 5.7/5.8 branch (affects interop expectations).
5. Terrain Diffusion code licence (weights are MIT) and Copernicus attribution inheritance.
6. Whether Atidarla's node canvas will be ready in time to reuse, or whether to start on `egui-snarl`.
7. Lab reality: share of students on base M-series MacBook Airs (8–16 GB) versus Pro/Max; whether UE runs on the same Mac or on Windows lab PCs.
8. Legal check of the plugin distribution model (source on GitHub + binaries on Fab).

---

## 10. Sources

**Gaea / QuadSpinner:** cgchannel.com/2026/06/quadspinner-releases-gaea-2-3 · cgchannel.com/2025/12/quadspinner-unveils-gaea-3-0 · blog.quadspinner.com/gaea-3-0-development-update · quadspinner.com/Gaea3 · nwdagroup.com/quadspinners-gaea-3-review · help.quadspinner.com/article/11-can-gaea-run-on-mac-or-linux · help.quadspinner.com/article/9-what-are-the-system-requirements-for-gaea · quadspinner.com/{Order,Compare,Download,Support/FAQ/Licensing,Order/Education/Students,Order/Education/Classroom,Gaea/Unreal,Gaea/Features,Gaea/Workflow} · help.quadspinner.com/article/{39,72} · docs.quadspinner.com/Guide/Build/Tiled.html · docs.quadspinner.com/Guide/Developers/API.html · docs.quadspinner.com/Learning · docs.quadspinner.com/Guide/Getting-Started/Your-First-Terrain.html · blog.quadspinner.com/{gaea-2-2-released,gaea2unreal-now-open-source,advanced-features-variables-compute-and-expressions} · digitalproduction.com/2025/07/18/gaea-2-2-unveils-frosty-nodes-and-selective-erosion · cgchannel.com/2024/08/quadspinner-releases-gaea-2-0 · github.com/QuadSpinner/Gaea2Unreal · eadocs.quadspinner.com/gaea-unreal-tools/importing-terrains · docs.gaea.app/guides/use-in/bridges/gaea2unreal/importing-terrains.html

**Other terrain tools:** help.world-machine.com/topic/build-4065-dragontail-peak · forum.world-machine.com (8510, 6424, 6430, 5987, 4641) · cgchannel.com/2026/07/world-machine-software-ships-world-machine-dragontail-peak · world-machine.com · cgchannel.com/2026/10/bitethebytes-unveils-world-creator-4 · cgchannel.com/2026/04/world-creator-2026-4-is-out-with-a-new-free-community-edition · cgchannel.com/2026/01/bitethebytes-releases-world-creator-2026-1 · cgchannel.com/2023/11/bitethebytes-releases-world-creator-2023 · docs.world-creator.com (unreal-bridge; world-creator-story) · cgchannel.com/2025/07/jangafx-releases-geogen-0-5 · 80.lv/articles/instant-terra-2-5-released · sidefx.com/products/whats-new-in-h22/world-building · sidefx.com/docs/houdini/heightfields_cop · sidefx.com/docs/houdini/unreal/{attributes,landscape/basics}.html · github.com/sideeffects/HoudiniEngineForUnreal(-v2)/releases · extensions.blender.org/add-ons/terrainmixer · docs.blender.org (A.N.T. Landscape; geometry nodes inspection; node groups) · github.com/otto-link/Hesiod · hesioddoc.readthedocs.io · sourceforge.net/app/terraforge3d · godotengine.org/asset-library/asset/3892 · github.com/kaylendog/kosmos

**Unreal Engine:** unrealengine.com/news/unreal-engine-5-8-is-now-available · unrealengine.com/news/state-of-unreal-2026-top-news-from-the-show · gamefromscratch.com/unreal-engine-5-8-released · dev.epicgames.com/documentation: unreal-engine-5-8-release-notes · crafting-mesh-terrain-in-unreal-engine · landscape-edit-layers-in-unreal-engine · landscape-technical-guide-in-unreal-engine · creating-landscapes-in-unreal-engine · importing-and-exporting-landscape-heightmaps-in-unreal-engine · landscape-patch-system · landscape-blueprint-brushes-in-unreal-engine · remote-control-api-{http,websocket}-reference-for-unreal-engine · macos-development-requirements-for-unreal-engine · API: ULandscapeEditLayerBase, ULandscapeEditLayerProcedural, ULandscapePatchEditLayer, ULandscapeTexturePatch, FLandscapeEditDataInterface, FMergeRenderContext, ALandscapeBlueprintBrushBase, IWebSocket::SetTextMessageMemoryLimit · python-api LandscapeTexturePatch · BlueprintAPI LandscapeImport{Heightmap,Weightmap}fromRenderTarget · issues.unrealengine.com/issue/UE-352957, UE-93368 · forums.unrealengine.com threads 2729274, 2707493, 2730153, 2739327, 2673913, 2689273, 2681879, 2512261, 2652298, 2702247, 2706407, 1697513, 770270, 414873 · unrealengine.com/eula/unreal · Cesium for Unreal (Fab, GitHub)

**Algorithms:** iquilezles.org/articles/warp · jcgt.org/published/0011/01/02 · history.siggraph.org (Worley 1996; Musgrave 1989; glacial 2023) · hgpu.org/?p=2256 (Mei 2007) · diglib.eg.org (Šťava 2008; Cordonnier 2016, 2018; Tzathas 2024; FastFlow 2024; Lochner 2023) · cs.purdue.edu/homes/bbenes/papers/{Stava08SCA,Cordonier16CGF,Genevaux13ToG,Guerin17ToG}.pdf · hal.science/hal-04049125 · h-schott.github.io (uplift, mserosion, desc, isos) · github.com/H-Schott/StreamPowerErosion · research.adobe.com/publication/terrain-amplification-using-multi-scale-erosion · github.com/SebLague/Hydraulic-Erosion · github.com/henrikglass/erodr · github.com/r-barnes/Barnes2013-Depressions · pro.arcgis.com flow-direction docs · cs.purdue.edu/cgvlab (Yang 2024, 2025) · jordanwalker98.github.io/projects/procedural-erosion.html · arxiv.org/abs/2512.08309 · hf.co/xandergos/terrain-diffusion-{30m,90m} · hf.co/NewtNewt/MESA · hf.co/joe-gibbs/terrainsr · CVPR 2025 W MESA paper · Copernicus DEM licence PDF · opentopography.org

**Learning:** Lytle et al. 2019 (academia.edu/42119136) · static.teachcomputing.org/pedagogy/QR11-PRIMM.pdf · eprints.gla.ac.uk/229012 · hg.gatech.edu/node/603172 · par.nsf.gov/servlets/purl/10510493 · Kalyuga et al. 2003 · dblp Sorva13 · worrydream.com/LearnableProgramming · Green & Petre 1996 · ccl.northwestern.edu/2017/a3_weintrop_wilensky.pdf · Resnick et al. 2009 · orbi.uliege.be (eCAADe 2025) · cumincades.scix.net/paper/sigradi2011_368 · dev.epicgames.com blueprint-debugger · docs.derivative.ca/OP_Snippets · learn.derivative.ca · sidefx.com/tutorials/houdini-foundations-book · helpx.adobe.com Substance exposed parameters · project.pages.drupalcode.org/modeler/replay · Valve AnimGraph debugging wiki

**Platform:** developer.apple.com/metal/Metal-Feature-Set-Tables.pdf · developer.apple.com/metal/Metal-Shading-Language-Specification.pdf · developer.apple.com/documentation/metal (maxBufferLength; storage modes; MTL4CommandQueue residency) · developer.apple.com/videos/play/wwdc2025/205 · wwdc2026/330 · wwdc2026/359 · apple.com/newsroom/2026/08 (M6, M5 Ultra) · macrumors.com (M5 Pro/Max, M4 Max) · appleinsider.com · github.com/shader-slang/{slang,slang-rhi,slangpy} · khronos.org Slang initiative press release · github.com/gfx-rs/wgpu (v30.0.1 features.rs) · github.com/tracel-ai/cubecl · github.com/ml-explore/mlx · github.com/modular/modular · llvm.org/devmtg/2026-04 · github.com/halide/Halide · github.com/taichi-dev/taichi · github.com/AdaptiveCpp/AdaptiveCpp · crates.io (objc2-metal, cudarc, egui, egui-snarl) · github.com/{thedmd/imgui-node-editor,Nelarius/imnodes,paceholder/nodeeditor} · doc.qt.io/qt-6/licensing.html · github.com/ProjectPhysX/FluidX3D · arxiv.org/abs/2502.05317
