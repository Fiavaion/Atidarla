# Atidarla — PRD.md

**Version:** 1.2 (A4, A6, A7, A10, A18, A19 ratified) · **Date:** 15 October 2026 · **Owner:** Mark
**Companion:** `RESEARCH.md` v1.0 (evidence for every decision below; section refs as `R§n`)
**Supersedes:** NascD PRD v0.4 (11 July 2026)
**Execution:** Claude Code, milestone by milestone (M0–M6, then M7+ horizon). Each milestone ends only when every acceptance criterion passes.

> **Atidarla** is a free, open-source real-time visual instrument for Windows and macOS (Linux best-effort): patch it like TouchDesigner, play it like Resolume, grow it like Houdini — with AI you can perform.

---

## 0. Decision log

Status key: **Ratified** = decided by Mark · **Carried** = ratified for NascD and still valid · **Proposed** = recommended by this PRD, needs Mark's yes/no before M0 closes.

| # | Decision | Status | Notes |
|---|---|---|---|
| A1 | Project name **Atidarla**; project folder `*.atidarla/`; CLI `atidarla`; Python module `atidarla` | Ratified 2026-10-15 | |
| A2 | Atidarla supersedes NascD; NascD docs are archived reference | Ratified 2026-10-15 | |
| A3 | Open source under Mark's company, public GitHub; NCAD is first adopter | Ratified 2026-10-15 | |
| A4 | Core licence **MIT OR Apache-2.0** (dual); GPL/NC components only as separate processes or optional plugins | Ratified 2026-10-15 | R§4.9, R§5.2 |
| A5 | Stack: Rust core, **wgpu 30.x** (pinned per release), **egui 0.37 + custom node canvas**, **PyO3 0.30**, Python 3.13 default (floor 3.11), WGSL + WESL | Carried (NascD D2) + version update | R§5.1 |
| A6 | All torch/TensorRT/ComfyUI/model work runs in **out-of-process AI Hosts** (local, LAN or cloud) under a supervisor | Ratified 2026-10-15 (revises NascD) | R§5.1, gap 6 |
| A7 | AI hardware **tiering** replaces "NVIDIA-only real-time": local real-time diffusion is a **Windows + NVIDIA platform capability** (same treatment as the Kinect sensor family); macOS/AMD/Intel get local interactive rates and real-time via LAN or cloud hosts | Ratified 2026-10-15 (replaces NascD D5) | R§4.1, R§4.6 |
| A8 | LLMs: **Ollama default** (local); Anthropic and other cloud providers explicit opt-in per project; **Sovereign mode** (no traffic beyond machine/LAN) is the default for new projects | Carried (NascD D6) + extended | R§4.7 |
| A9 | Three peer views over one graph: **Patch**, **Stage**, **Timeline** | Proposed | Differentiator D1 |
| A10 | Eight operator families mapped to TouchDesigner: **IMG** (TOP), **SIG** (CHOP), **PNT** (POP, GPU), **MSH** (SOP, CPU topology), **MAT**, **DAT**, **CMP** (COMP), **AI**; PNT and MSH share one GEO wire type with automatic GPU/CPU residency; **~200 operators by v1.0** (registry §6) | Ratified 2026-10-15 (replaces ~176-op NascD registry and the ~125 draft) | R§2.2, R§2.3, Mark |
| A11 | Project format = **folder of text files** (one file per network/Pack, stable IDs); binary only for caches | Ratified 2026-10-16 (revises NascD `.nascdata`) | Gap 5; layout §5.5, TOML |
| A12 | **Kernel DSL** (VEX-flavoured) lowered via typed IR to WGSL; raw WGSL escape-hatch node | Proposed | R§5.4 |
| A13 | Adopt differentiators **D1–D10, D12–D14 for v1.0**; **D11 (multi-machine), D15 (web player) in M7** | Proposed | R§7 |
| A14 | Ableton Link and NDI ship as **optional plugins** (licence isolation, dynamic load) | Proposed | R§5.2 |
| A15 | Carried NascD additions: ISF import, HAP-first codecs, headless/offline bake, OSCQuery, built-in MCP server | Carried (NascD D3) | |
| A16 | Lab performance baseline **RTX 4070 12 GB**; low-spec guard rail = GTX 1660-class / Apple M1 with AI disabled | Carried | |
| A17 | No telemetry; crash reports opt-in, written locally first | Carried | |
| A18 | **Platform priority: Windows and macOS are the supported targets; Linux is nice-to-have** — kept compiling where cheap, never gates a milestone | Ratified 2026-10-15 | Mark |
| A19 | **Platform-exclusive capabilities are allowed** when hardware or SDKs require it (e.g. NVIDIA real-time diffusion, Kinect/Orbbec, Spout vs Syphon); each is declared in the operator registry as a capability flag, shown in the UI, and degrades gracefully (route to LAN/cloud, hide, or substitute) on other platforms | Ratified 2026-10-15 (follows A7) | Mark |
| A20 | **In-house dock widget in `atd-app`** until `egui_dock` supports egui 0.37 (it and `egui_tiles` target 0.36 as of 2026-10-15); same API shape so it can be swapped back. Keeps the A5 pins (egui 0.37, wgpu 30) intact | Proposed 2026-10-15 (M0) | `docs/milestones/M0.md` |
| A21 | **M1 AC2 corpus = the public ISF collection as it exists** (Vidvox/ISF-Files at `395072d`, 327 shaders), replacing the "500 shaders" in §9; the ≥90% bar is unchanged | Ratified 2026-10-16 | The collection has 327 shaders; `docs/milestones/M1.md` |
| A22 | **Expressions (FR-P2) in two tiers:** a native expression subset (arithmetic, maths functions, `time`/`frame`, `me.digits`, parameter and channel references) evaluated in Rust on the cook path; any other expression runs as Python on the Python worker thread, its result latched for the next frame (one frame late, marked in the UI). The render thread never waits on Python (rule 1) | Ratified 2026-10-16 | `docs/milestones/M1.md` |
| A23 | **Video packaging:** FFmpeg as an **LGPL build** (no `--enable-gpl`/`--enable-nonfree`), **dynamically linked** and shipped as replaceable shared libraries beside the app; hardware decode through the platform APIs (D3D11VA/NVDEC on Windows, VideoToolbox on macOS) with software fallback. HAP stays native (no FFmpeg) | Ratified 2026-10-16 | A4, rule 3; `docs/milestones/M1.md` |

---

## 1. Problem and vision

Visual artists today assemble a show from three or four paid, partly-incompatible tools: a node engine (TouchDesigner, $300–2,200, no Linux, 1280² cap on the free tier), a performance layer (Resolume Arena €799, no Linux, patching split into a separate 2D-only app), a procedural/simulation tool (Houdini, up to ~$4.5k, offline), and one or more AI apps piped in over Spout/NDI with no tempo, MIDI or clip semantics. Schools pay per seat and AI is bolted on.

Atidarla is one open application where the same graph is **patched** (node editor), **played** (clip deck with outputs) and **sequenced** (timeline), with procedural GPU geometry and live AI as first-class citizens — on Windows and macOS, free, with no caps.

---

## 2. Users and personas

1. **Student / learning artist** (NCAD primary): lab RTX 4070s or a personal laptop; needs approachable UI, Learn mode, AI that explains and builds; zero licensing.
2. **VJ / live performer:** needs a reliable clip deck, BPM sync, MIDI/OSC mapping, low-latency outputs, show-safe AI.
3. **Installation / interaction designer:** sensors (Kinect, cameras, OSC), projection mapping, DMX/LED, kiosk mode with watchdog, scripting.
4. **Technical artist / developer (incl. Mark):** Python and Kernel DSL, Packs, headless CLI, MCP control from Claude Code, golden-image deployment.
5. **Educator:** reproducible projects, Git-friendly assignments, Learn-mode quests, AI estate on the LAN.

---

## 3. Goals and non-goals

**Goals (v1.0)**
- **G1** TouchDesigner-class node engine: operator families, lazy pull cook, expressions, exports, nested networks, Packs.
- **G2** Resolume-class performance layer: clip deck, BPM, effects, advanced output, DMX, MIDI/OSC.
- **G3** Houdini-inspired procedural core: attribute model, Kernel DSL, solvers, Batch Graph.
- **G4** Native AI: live diffusion/video as clips and nodes, vision ML, LLM assistant, ComfyUI integration, hybrid local/LAN/cloud routing.
- **G5** Windows and macOS parity (native Metal on Mac), with platform-exclusive capabilities flagged rather than blocked (A19); Linux best-effort (A18).
- **G6** Show reliability: crash isolation, Output Governor, soak-tested.
- **G7** Git-native, agent-friendly project format.

**Non-goals (v1.x)**
- Operator-count parity with TouchDesigner (600+); v1.0 ships ~200 core operators and breadth comes from Python ops, Kernel DSL, ISF and Packs.
- Offline film-quality rendering (Karma/Notch-class), DOP-style simulation framework, FLIP/sparse pyro/RBD (import bakes instead).
- Non-linear video editing; mobile; FFGL hosting (M7+); training models inside the app.
- Linux as a supported, release-gating platform (best-effort only, A18).
- Bundling any model weights.

---

## 4. Differentiators (summary — detail in R§7)

| ID | Differentiator | Milestone |
|---|---|---|
| D1 | Patch · Stage · Timeline in one app over one graph | M1–M3 |
| D2 | Free with no caps, no keys, no Pro tier — sync, mapping and output tools included; Windows + macOS parity | M0 onward |
| D3 | AI clips & AI layers + Output Governor | M4 |
| D4 | Hybrid compute router (local/LAN/cloud) with meter + Sovereign mode | M4 |
| D5 | Show-safe agent: typed MCP, diff review, Show Lock, in-show cues | M5 |
| D6 | Git-native projects with visual diff and node merge | M1, M5 |
| D7 | Kernel DSL with state-preserving hot reload | M2 |
| D8 | Batch Graph (PDG-style wedging for AI) | M5 |
| D9 | Bring-your-own ComfyUI workflow as a node | M4 |
| D10 | Crash-isolated show engine | M4, M6 |
| D11 | Free multi-machine sync | M7 |
| D12 | Capture-to-stage (splats, Kinect/volumetric, VAT, point caches) | M6 |
| D13 | Learn mode (quests, explain-this-network) | M5 |
| D14 | Licence-aware model management | M4 |
| D15 | Web Player export | M7 |

---

## 5. System architecture

### 5.1 Process model

```
┌───────────────────────── atidarla (main process) ─────────────────────────┐
│  UI thread (egui + node canvas)  ◄──commands/undo──►  Graph store          │
│  Cook/render thread (wgpu) — owns frame loop, never blocks on AI/Python    │
│  I/O threads (video decode, audio, MIDI/OSC/DMX, network)                  │
│  Python interpreter (PyO3) — scripting & Python ops on a worker thread     │
│  Supervisor — spawns/monitors AI Hosts, restarts on failure                │
└───────────────┬─────────────────────────────────────────────┬─────────────┘
                │ AI Host Protocol (control: JSON-RPC over     │
                │ local socket/named pipe; frames: GPU handle  │
                ▼ or shared-memory ring)                       ▼
   ┌─────────────────────────┐   ┌──────────────────────────┐   ┌───────────────────┐
   │ Local AI Host (Python)  │   │ LAN AI Host (same proto, │   │ Cloud adapters     │
   │ torch/TensorRT/ONNX/    │   │ frames over WebRTC/NDI/  │   │ Decart, Daydream,  │
   │ Core ML/MLX; ComfyUI    │   │ raw TCP; e.g. NCAD DGX/  │   │ fal, Runpod, Comfy │
   │ connector               │   │ Threadripper)            │   │ Cloud, Anthropic   │
   └─────────────────────────┘   └──────────────────────────┘   └───────────────────┘
```

### 5.2 Crates (Cargo workspace)

| Crate | Responsibility |
|---|---|
| `atd-core` | Graph store, typed sockets, parameter system (float/int/string/menu/toggle/pulse/colour/vecN/path + expression slot), dirty-flag pull cook scheduler, time model, undo/redo command log, operator registry + JSON-Schema export |
| `atd-gpu` | wgpu device/queue, texture & buffer pools, per-frame render-graph compile, WGSL/WESL library, attribute-buffer framework, Kernel DSL compiler (`atd-kernel` sub-crate), ISF→WGSL via `naga` |
| `atd-ops-*` | Operator implementations per family (`img`, `sig`, `pnt`, `msh`, `dat`, `mat`, `cmp`, `ai`) |
| `atd-io` | Video (FFmpeg-LGPL, HAP, Vulkan Video, VideoToolbox), audio (cpal), cameras, OSC/OSCQuery, MIDI 1/2, Art-Net/sACN, LTC/MTC, serial, HTTP/WebSocket |
| `atd-interop` | Per-OS GPU sharing via `wgpu-hal`: Spout + CUDA external memory/IPC (Windows), Syphon/IOSurface (Mac); readback fallback. Linux (libfunnel/PipeWire, dmabuf) best-effort |
| `atd-plugin-*` | Optional separately-licensed plugins: `ndi`, `link` (Ableton Link, GPL) |
| `atd-py` | PyO3 bindings: `atidarla` module, TD-familiar API (`op('/proj/noise1').par.period = 2`), callbacks, custom-op base classes with schema-derived UI |
| `atd-ai` | AI Host protocol, supervisor, compute router, provider adapters, model manager (licence badges, downloads), Output Governor |
| `atd-agent` | LLM assistant backend, MCP server (typed tools, permissions, Show Lock), diff/review engine |
| `atd-stage` | Clip deck, BPM/clock, transitions, autopilot, advanced output (slices, masks, warp, blend), DMX fixtures & pixel mapping |
| `atd-timeline` | Keyframes, cues, triggers/conditions, timecode chase |
| `atd-format` | Project folder read/write, stable IDs, diff and three-way merge, show bundle export |
| `atd-app` | Application shell: Patch/Stage/Timeline views, viewers, parameter panel, textport/REPL, palette, Learn mode |
| `atd-cli` | `atidarla run|render|bake|serve|bench` headless entry points |
| `ai-host/` (Python package) | Local AI Host implementation (uv-managed env), model adapters |

### 5.3 Cook model
- Lazy, pull-based: outputs (Stage outputs, viewers, Out nodes, exports) request data per frame; nodes cook if dirty or time-dependent.
- Each frame the cook pass compiles requested GPU work into a render graph; static subgraphs are cached; **Compiled Blocks** fuse eligible kernels into single dispatches.
- Frame-critical paths allocate nothing after warm-up; per-node cook time and GPU timestamp metrics are always on.
- AI nodes are **asynchronous**: they submit frames to a Host and output the latest result; the Output Governor decides hold/blend/interpolate so the output frame rate never drops.

### 5.4 Data types on wires
`IMG` (GPU texture: format, size, colour space, typed layer: Mono/UV/RGB/RGBA/ID, metadata) · `SIG` (channels × samples with rate) · `GEO` (attribute buffers: point/prim/detail, groups, topology; produced and consumed by both PNT and MSH; residency GPU/CPU tracked and transferred automatically) · `DAT` (table/text/JSON) · `MAT` (material handle) · `CMP` (scene object/container ref). Cross-family conversion only through explicit converter nodes; parameters can reference any family via expressions or exports.

### 5.5 Project format (`*.atidarla/`)
```
myshow.atidarla/
  project.toml            # metadata, settings, model manifest, AI routing policy
  networks/<id>.toml      # one file per network: nodes, params, wires (sorted, stable IDs)
  packs/<name>/pack.toml  # Pack definition + nested networks + help.md
  stage/deck.toml         # compositions, layers, clips, mappings
  timeline/*.toml
  scripts/*.py  kernels/*.atk  shaders/*.wgsl|*.fs (ISF)
  media/ (references or copies)  .cache/ (gitignored binaries)
```

---

## 6. Operator registry (v1.0 target ≈ 200)

Eight families, mapped closely to TouchDesigner so TD users find their way, with Houdini's SOP core restored as its own family. Each operator ships with: schema (params, inputs, outputs, capability flags), docs page, one runnable example, unit test, cook-time budget. Names are Atidarla's own (no TD trademarks). Breadth beyond v1.0 comes from Python ops, Kernel DSL, ISF and Packs.

| Atidarla family | TouchDesigner analogue | Houdini analogue | Runs on | v1.0 count |
|---|---|---|---|---|
| IMG | TOP | COP / Copernicus | GPU | ≈49 |
| SIG | CHOP | CHOP | CPU (audio rate) / GPU where useful | ≈30 |
| PNT | POP | SOP point ops, POPs | GPU | ≈38 |
| MSH | SOP | SOP (topology/modelling) | CPU topology + GPU upload | ≈34 |
| MAT | MAT | VOP materials | GPU | ≈8 |
| DAT | DAT | — | CPU | ≈13 |
| CMP | COMP | OBJ / HDA containers | — | ≈10 |
| AI | — (third-party in TD) | ML / ONNX nodes | AI Hosts | ≈16 |

**One geometry wire, two families.** PNT and MSH both read and write the same `GEO` wire type (§5.4), so they connect directly with no conversion nodes (a TD pain point). PNT operators are GPU-resident and work on any GEO (they deform meshes without changing topology). MSH operators change topology on the CPU; the engine tracks where each GEO lives (GPU/CPU), performs readback/upload automatically, and shows a residency badge and transfer cost on the wire. MSH cooks run on worker threads and never block the render thread; a time-dependent MSH network over its frame budget gets a warning.

**IMG — GPU image (≈49)**
- *Generators & I/O:* Constant, Ramp, Noise, Text, Shape, Movie In, Movie Out (record), Camera In, Screen Capture, Share In, Share Out (Spout/Syphon), NDI In, NDI Out (plugin).
- *Compositing & colour:* Composite, Blend, Level, HSV Adjust, Channel Mix, Lookup (LUT), Threshold, Remap, Chroma Key, Luma Key.
- *Filters & distortion:* Blur, Edge, Normal Map, Slope, Displace, Bloom, Mirror, Tile, Kaleidoscope, Projection (equirect/cubemap/fisheye).
- *Transform & time:* Transform, Crop/Fit, Resolution, Feedback, Switch, Cache (frame delay), Time Machine.
- *Render & code:* Render (GEO + MAT + camera), Kernel (pixel), WGSL, ISF, Analyze (→SIG).
- *Solvers:* Reaction-Diffusion, Fluid 2D, Ripple, Neural CA.

**SIG — channels (≈30)**
- *Generators:* Constant, LFO, Noise, Pattern, Timer, Trigger, Count, Beat (BPM clock).
- *Processing:* Math, Expression, Lag, Filter, Envelope, Speed (integrate), Limit, Select, Merge, Trail (history).
- *Audio:* Audio In, Audio File In, Audio Out, Spectrum, Onset/Beat Detect.
- *Devices & protocols:* MIDI In, MIDI Out, OSC In, OSC Out, DMX Out (Art-Net/sACN), Input Devices (keyboard/mouse/gamepad), Timecode In (LTC/MTC).

**PNT — GPU points (≈38; NascD's POP design restored and updated with TD's 2025–26 POP additions)**
- *Generators & import:* Point Shape (grid/sphere/box/line/spiral), Scatter (on surface/in volume), Points from Image, Points from Depth, Point File In (PLY/CSV/point caches; `.bgeo`-class via converter), Splat In (PLY/SPZ/SOG), Sensor In (Kinect/Orbbec; capability-flagged).
- *Attributes:* Attribute Create, Attribute Noise, Attribute Math, Attribute from Image (IMG lookup), Attribute from Channels (SIG lookup), Attribute Transfer, Attribute Promote, Kernel (point/prim).
- *Deformer stack (Notch-style effectors):* Transform, Deform (bend/twist/taper), Noise Deform, Lattice, Falloff Field.
- *Spatial & structure:* Merge, Group, Delete, Sort, Neighbour (k-NN), Proximity, Ray Intersect, Trail, Resample, Copy to Points/Instance.
- *Simulation:* Particles (emit/age/kill), Forces (curl noise, attract, turbulence, drag, vortex), Flock, Collide (SDF/plane/mesh), Solver Block, For-Each.
- *Outputs:* Pixel Out (points → DMX/LED/laser), To Channels (→SIG).

**MSH — mesh / modelling (≈34; the SOP family)**
- *Primitives & import:* Box, Sphere, Grid, Tube, Torus, Circle, Line/Curve, Text 3D, Mesh File In (glTF/OBJ/PLY/Alembic; USD import), VAT In (Houdini VAT 3.0).
- *Modelling:* Extrude, Bevel, Boolean, Subdivide, PolyReduce, Remesh, Smooth, Fuse, Clean, Normals, Mirror, Clip, Triangulate.
- *Construction:* Sweep, Revolve, Skin (curves → surface), Carve, Iso Surface (SDF/volume → mesh), Convex Hull, Fracture (Voronoi; cooks off-frame, typically baked).
- *UV & analysis:* UV Project, UV Unwrap, Measure (area/curvature/normals).
- *Output:* Mesh File Out (glTF/OBJ/PLY).
- *Candidate libraries (verify licences in M0):* Manifold (booleans, Apache-2.0), meshoptimizer (PolyReduce, MIT), xatlas (UV Unwrap, MIT), Geogram (remeshing, BSD-3).

**MAT (≈8):** PBR, Unlit, Matcap, Toon, Point Sprite, Line, Splat, WGSL Material.

**DAT (≈13):** Table, Text, JSON, Script (Python), File In, File Out, Web Client (HTTP), Web Server, WebSocket, Serial, Convert, Select, Prompt Template.

**CMP (≈10):** Container (sub-network), Pack, Panel (UI root), Widgets (button/slider/field/XY/list), Window (output window), Camera, Light, Environment (HDRI), Scene Object, Replicator.

**AI (≈16):** Diffusion (img2img/txt2img, StreamDiffusion techniques; `win+nvidia` for local real-time), Prompt Blend, Control Prep (depth/canny/pose/lineart), Video Model (streaming V2V), ComfyUI Workflow, ONNX Inference (IMG and GEO variants), Depth, Segment, Pose, Optical Flow, Background Remove, Upscale, Speech-to-Text, LLM Chat, Agent, Generate Asset (batch image/3D → media bin).

---

## 7. Functional requirements

IDs are stable; each maps to a milestone. "MUST" = required for that milestone's acceptance.

### 7.1 Patch (node engine)
- **FR-P1** Node canvas: pan/zoom 60 fps with 2,000 nodes; wire routing; multi-select; copy/paste between projects; search-to-add (fuzzy, by schema keywords); comment boxes; minimap. (M1)
- **FR-P2** Parameter panel generated from schema; expression slot on every parameter (native subset on the cook path, other Python one frame late on the worker thread: A22); channel export (SIG → any parameter) by drag. (M1)
- **FR-P3** Viewers per family (texture with channel/alpha/HDR, channel plot, geometry 3D with attribute spreadsheet, table). (M1–M2)
- **FR-P4** Nested networks and Packs: promote parameters, semantic version, embedded help, lock/unlock, update-in-place across project. (M2)
- **FR-P5** Textport/REPL bound to `atidarla` Python module. (M2)
- **FR-P6** Perform mode: a Panel CMP shown full-screen with UI only. (M3)
- **FR-P7** Palette browser of shipped Packs and examples. (M2)

### 7.2 Kernel DSL and scripting
- **FR-K1** Kernel nodes over points, prims, pixels and voxels; `@attr` read/write with schema-generated bindings; `ch()` parameter hooks auto-create UI. (M2)
- **FR-K2** Helpers: `point()`, `nearpoints()`, noise family (perlin/simplex/curl/worley), math, matrix/quaternion, sampling of input IMG. (M2)
- **FR-K3** Compile errors shown inline with line/col; "Generated WGSL" view. (M2)
- **FR-K4** State-preserving hot reload of kernels and Python ops (sim state kept across edits). (M2)
- **FR-K5** Raw WGSL node with `#bind` declarations, tile size and iterations. (M2)
- **FR-K6** ISF import (JSON header → params; GLSL → WGSL via naga; failures reported per shader). (M1)

### 7.3 Stage (performance layer)
- **FR-S1** Composition → decks → layers × columns; launch modes (trigger, toggle, flash, one-shot), legato, column launch, layer groups, crossfader A/B. (M3)
- **FR-S2** Clip sources: media, live input, any network or Pack, AI clip (M4). Clips have thumbnails (live-updating for generators). (M3)
- **FR-S3** Effects chains at clip/layer/group/composition level using any IMG node or Pack; blend modes; per-clip transitions. (M3)
- **FR-S4** Clock: tap, audio beat detection, MIDI clock in/out, Ableton Link (plugin); beat-synced LFOs, transitions and quantised launch. (M3)
- **FR-S5** MIDI/OSC learn on any parameter with ranges, curves and toggles; live input monitors; OSCQuery advertising. (M3)
- **FR-S6** Advanced Output: multiple outputs/windows/displays; slices; masks; grid and bezier warp; soft edge blending; test patterns; 10-bit/HDR where supported. (M3)
- **FR-S7** DMX: Art-Net and sACN out; fixture library; LED pixel mapping from IMG regions. (M3)
- **FR-S8** Autopilot (next/random/specific column after N beats or clip end). (M3)
- **FR-S9** Show export: single portable bundle with all referenced media, Packs and model manifest; import on another OS without path fixes. (M3)
- **FR-S10** Record composition output to file (HAP/ProRes/H.264/H.265). (M3)

### 7.4 Timeline
- **FR-T1** Keyframe tracks for any parameter; curves; markers. (M3)
- **FR-T2** Cues that trigger clips/columns/parameter states; interactive triggers and conditions (ossia-style). (M3)
- **FR-T3** Chase LTC/MTC; drive timeline from Stage or vice versa. (M3)

### 7.5 AI
- **FR-A1** AI Host Protocol v1: session open/close, capabilities query (vendor, VRAM, runtimes), model load/unload, frame submit/receive with sequence IDs and timestamps, health/heartbeat, cancel. Frame transport: CUDA IPC/external memory (Windows + NVIDIA), shared-memory ring (all), network codec (LAN/cloud); dmabuf on Linux best-effort. (M0 prototype, M4 final)
- **FR-A2** Supervisor: auto-start hosts on demand, restart on crash within 5 s, surface state in UI; output holds last good AI frame meanwhile. (M4)
- **FR-A3** Diffusion node with StreamDiffusion-class techniques (stream batch, residual CFG, similarity filter, tiny VAE, TensorRT engine cache), SD-Turbo/SDXL-Turbo/LCM-family adapters, ControlNet and IP-Adapter inputs, LoRA weights as parameters, seed and prompt as parameters. **Local real-time path is capability-flagged `win+nvidia` (A7/A19)**; on other platforms the node routes to a LAN/cloud host automatically or runs at interactive rates. Clean-room implementation from papers and Apache-2.0 sources only. (M4)
- **FR-A4** Prompt Blend: weighted blend of N prompt embeddings, driveable by SIG/crossfader. (M4)
- **FR-A5** AI clips in Stage: prompt clips, model/LoRA per clip, crossfader blends prompts between A/B, seed change on beat/quantised launch. (M4)
- **FR-A6** Output Governor: per AI node policy — hold, cross-blend, optical-flow interpolation, or passthrough-with-effect — keeping output at the project frame rate; shows effective AI fps. (M4)
- **FR-A7** Compute router: per-node policy list (e.g. `local → lan:ncad-dgx → cloud:decart`), fallback on failure/latency budget; badges show where each node ran, latency, VRAM, est. cost; per-project cloud spend cap. (M4)
- **FR-A8** Sovereign mode: blocks all non-local/non-LAN AI traffic; default ON for new projects; clear indicator in title bar. (M4)
- **FR-A9** Vision ML via ONNX Runtime with best EP per OS (TensorRT-RTX/Windows ML/CUDA/ROCm/MIGraphX/Core ML/OpenVINO/CPU): Depth, Segment, Pose (MediaPipe fallback everywhere), Optical Flow, Background Remove, Upscale, generic ONNX Inference. (M4)
- **FR-A10** ComfyUI Workflow node: load API-format JSON; derive parameters from `/object_info`; run against configured ComfyUI server (local or LAN); stream progress; result to IMG or media bin. Never bundled or linked. (M4)
- **FR-A11** Video Model node (streaming V2V) via local host (24 GB+ NVIDIA) or LAN/cloud. (M4)
- **FR-A12** Model manager: per-model licence badge (commercial-safe / restricted / non-commercial), on-demand download after explicit licence acceptance, checksums, per-project manifest; "commercial-safe only" project filter. (M4)
- **FR-A13** Speech-to-Text (Whisper) and LLM Chat (Ollama default; Anthropic and others opt-in). (M5)

### 7.6 Agent (show-safe)
- **FR-G1** MCP server (stdio + local HTTP) exposing typed tools: `list_ops`, `get_schema`, `create_node`, `connect`, `set_param`, `create_pack`, `write_kernel`, `run_bench`, `screenshot_viewer`, `stage_*`. No arbitrary code execution unless the user enables the `python.exec` scope per session. (M5)
- **FR-G2** Every agent change is staged as a diff (visual node diff + param table) for accept/reject; accepted batches are one undo step. (M5)
- **FR-G3** Show Lock: during performance, agents may only adjust whitelisted parameters/Packs within declared ranges; graph edits refused. (M5)
- **FR-G4** In-app assistant panel: prompt-to-network (generate → instantiate → read errors → repair loop), "explain this network/node", grounded in generated operator docs. (M5)
- **FR-G5** In-show cues: text or voice ("darker, slower on the next drop") mapped to parameter moves within Show Lock bounds, quantised to the clock. (M5)

### 7.7 Batch Graph
- **FR-B1** Wedge node over prompt/seed/LoRA/ControlNet weights/any parameter; work items carry attributes; fan-out/fan-in. (M5)
- **FR-B2** Schedulers: local host, LAN hosts (round-robin by capability), ComfyUI servers, cloud queues. (M5)
- **FR-B3** Results cached by content hash; land in Stage media bin with metadata; re-runs recook only changed items. (M5)

### 7.8 Collaboration and format
- **FR-F1** Text project format (§5.5); deterministic serialisation; stable node IDs; round-trip tests. (M1)
- **FR-F2** Visual diff between two project states (node-level adds/removes/param changes). (M5)
- **FR-F3** Three-way merge of networks with conflict UI. (M5)

### 7.9 Capture pipeline and Learn
- **FR-C1** Splat In + Splat MAT rendering PLY/SPZ/SOG with sort-on-GPU; 1M splats at 60 fps on RTX 4070. (M6)
- **FR-C2** VAT 3.0 import (soft/rigid/fluid/sprite). (M6)
- **FR-C3** Kinect/Orbbec depth + body tracking via Sensor In, capability-flagged per SDK (Windows first). (M6)
- **FR-L1** Learn mode: at least 10 interactive quests (first patch, audio-reactive, first clip deck, Kernel DSL, particles, AI clip, mapping, DMX, Packs, ComfyUI). (M5)

### 7.10 Headless and deployment
- **FR-H1** `atidarla render project --out clip.mov --range 0-300 --fixed-step` deterministic offline render. (M2)
- **FR-H2** `atidarla run project --kiosk` with watchdog and auto-restart. (M3)
- **FR-H3** `atidarla serve` exposes MCP + OSC/REST without UI. (M5)
- **FR-H4** `atidarla bench` runs the standard performance suite (§8) and writes JSON. (M0 onward)
- **FR-H5** Silent install flags for Windows golden image (MSI/MSIX), signed + notarised macOS DMG. Linux AppImage best-effort. (M6)

---

## 8. Performance and quality targets

Measured by `atidarla bench` on the reference machines; regressions >10% fail CI on the self-hosted NVIDIA runner.

| ID | Target | Reference hardware |
|---|---|---|
| PT-1 | 4K60 output with a 20-node IMG chain (composite, blur, feedback, LUT, kernel) | RTX 4070 12 GB |
| PT-2 | 2M-particle PNT simulation (curl noise + attract + age) rendered at 1080p60 | RTX 4070 |
| PT-3 | SD-Turbo 512² img2img ≥30 fps (floor 24 fps; confirmed by M0 benchmark) while output holds 60 fps | RTX 4070 |
| PT-4 | Same pipeline ≥90 fps | RTX 4090 |
| PT-5 | 1080p60 compositing, 500k particles; AI interactive tier ≥5 fps local | Apple M-series (M2 Pro+) |
| PT-6 | 1080p30 with AI disabled | GTX 1660 / Apple M1 (guard rail) |
| PT-7 | Clip launch to first frame ≤1 frame for HAP; ≤3 frames for H.264/H.265 hardware decode | RTX 4070 |
| PT-8 | MIDI/OSC input → visible change ≤2 frames | all |
| PT-9 | AI Host crash → host restarted and producing frames ≤5 s; output never drops below project fps | all |
| PT-10 | 24-hour soak: no leak >100 MB, no frame-time spike >50 ms after warm-up | RTX 4070, Windows lab image |
| PT-11 | Cold start to interactive ≤5 s (excluding AI Host warm-up) | all |
| PT-12 | MSH: Boolean of two 100k-triangle meshes ≤150 ms; PolyReduce 1M→100k triangles ≤500 ms; cooks never stall output frames | RTX 4070 PC (CPU side) |

**Quality rules:** frame-critical paths allocation-free after warm-up; the render thread never waits on Python, AI Hosts or disk; GPU resource lifetimes tracked by pools; all public APIs documented; `cargo deny` blocks GPL/NC dependencies in core crates.

---

## 9. Milestones and acceptance criteria

### M0 — Foundations & spikes
- Cargo workspace with crates from §5.2 (stubs), CI on Windows and macOS (Apple Silicon) as required jobs, plus a non-blocking Linux build job; `cargo fmt`, `clippy -D warnings`, `cargo deny`.
- wgpu 30 window + egui 0.37 shell with dockable panels and an empty node canvas on Windows and macOS.
- Capability-flag mechanism in the operator registry (A19) with platform/vendor detection at startup.
- Spikes (each a short report in `docs/spikes/`): (a) Spout and Syphon texture sharing via `wgpu-hal`; (b) CUDA external-memory/IPC import to wgpu on Windows; (c) macOS signing + notarisation of an app bundle with an embedded Python and a separately signed AI Host helper; (d) AI Host Protocol prototype passing frames by shared memory and (NVIDIA) GPU handle; (e) SD-Turbo 512² StreamDiffusion-style benchmark on RTX 4070 (Windows) and Apple M-series (to size the interactive tier).
- **Acceptance:** CI green on Windows and macOS; shell runs on both; spike reports committed with numbers; remaining Proposed decisions (A5, A8, A9, A11–A14) ratified or amended by Mark; MSH library licences confirmed; PT-3 target confirmed or revised from spike (e).

### M1 — Graph engine, IMG & SIG
- `atd-core` cook engine, parameter system, expressions (Python via PyO3, cached), exports, undo/redo; text project format + round-trip tests.
- Node canvas (FR-P1), parameter panel (FR-P2), texture and channel viewers.
- IMG ≥30 ops and SIG ≥20 ops from §6; Movie In (HAP + FFmpeg + hardware decode on each OS); Share In/Out per OS; ISF import (FR-K6).
- **Acceptance:** audio-reactive feedback patch built by hand and saved/reloaded identically on Windows and macOS; the public ISF collection (327 shaders, A21) imports with ≥90% success; PT-1 met on RTX 4070; project diff in Git is human-readable.

### M2 — Procedural core: PNT, MSH, MAT, Kernel DSL, Python ops, Packs
- Attribute-buffer framework with GPU/CPU residency; PNT ≥30 ops (incl. particles, forces, Solver Block, For-Each, deformer stack); MSH ≥18 core ops (primitives, Mesh File In/Out, Extrude, Boolean, Subdivide, PolyReduce, Normals, Sweep, Skin, UV Project, Triangulate); MAT ≥6; Render IMG.
- Kernel DSL compiler (FR-K1–K5) with hot reload; Python custom ops with schema-derived UI; Packs (FR-P4); Palette with ≥15 Packs; textport.
- Headless deterministic render (FR-H1); 2D solvers (reaction-diffusion, fluid 2D, ripple, NCA).
- **Acceptance:** PT-2 met; a curl-noise particle system written entirely as a 10-line Kernel produces identical output to the node-built equivalent; editing a running kernel keeps simulation state; deterministic render produces byte-identical frames across two runs on the same machine; a PNT → MSH → PNT chain (scatter → Iso Surface/Boolean → noise deform) wires directly with no conversion nodes and shows residency badges; PT-12 met; Boolean → UV Project → Mesh File Out produces a glTF that opens in Blender.

### M3 — Stage & Timeline
- Clip deck (FR-S1–S3), clock (FR-S4; Link as plugin), MIDI/OSC learn + OSCQuery (FR-S5), Advanced Output (FR-S6), DMX + pixel mapping (FR-S7), autopilot, show export, recording, timeline + cues + timecode, perform mode, kiosk mode.
- **Acceptance:** a 60-minute scripted test set (8 layers, 64 clips, beat-synced effects, 2 outputs with slices/warp/blend, DMX to a test fixture) runs without frame drops on RTX 4070; show bundle exported on Windows plays unmodified on macOS (platform-flagged nodes report their substitute); PT-7 and PT-8 met.

### M4 — AI Hosts, live AI and routing
- AI Host Protocol v1, supervisor, local Python host (uv-managed env), model manager with licence badges.
- Diffusion, Prompt Blend, Control Prep, Video Model, vision ONNX nodes, ComfyUI Workflow node, AI clips, Output Governor, compute router (local → LAN → cloud) with meter and spend cap, Sovereign mode; cloud adapters: Decart, Daydream (if terms allow), Runpod-hosted container, fal; LAN host = same protocol on a remote machine.
- **Acceptance:** PT-3, PT-4, PT-9 met; prompt crossfade between two AI clips on the crossfader is smooth at 60 fps output; killing the AI Host mid-show never drops output frames; the same project opened on a Mac shows the `win+nvidia` badge on the Diffusion node, routes to a LAN Windows/NVIDIA host and reaches ≥24 AI fps; Sovereign mode verified by a packet-capture test showing no non-LAN traffic; a ComfyUI API-format workflow imports and runs with auto-generated parameters.

### M5 — Agent, Batch Graph, collaboration, Learn
- MCP server + typed tools + permissions (FR-G1–G3), assistant panel (FR-G4), in-show cues (FR-G5), Batch Graph (FR-B1–B3), visual diff + three-way merge (FR-F2–F3), Learn mode (FR-L1), `serve` mode, docs-as-code (operator pages generated from schema; same corpus grounds the assistant).
- **Acceptance:** 20-prompt network-generation eval ≥90% valid networks with Ollama default model and recorded results for the opt-in cloud model; Claude Code drives Atidarla via MCP to build a 15-node patch with no manual fixes; Show Lock blocks a graph edit in a scripted red-team test; a 100-item seed/prompt wedge completes across two LAN hosts and lands in the media bin; a conflicting merge resolves through the UI.

### M6 — Capture pipeline, hardening, v1.0
- Registry completed to ~200: remaining MSH (Bevel, Remesh, Fracture, UV Unwrap, Carve, Convex Hull, Iso Surface), IMG and SIG ops; Splat In/MAT, VAT In, Sensor In, point-cache converter; signed installers for Windows and macOS; Windows golden-image silent install; 24-hour soak; accessibility pass (keyboard navigation, UI scaling, colour-blind-safe wire colours); user manual; example library (≥40 examples).
- **Acceptance:** all PT targets met; PT-10 soak passes on the NCAD lab image; NCAD capture-rig splat renders at 60 fps (FR-C1); install → first patch in under 10 minutes for a new student (5-person usability test); v1.0 tagged and released on GitHub.

### M7+ — Horizon
Linux as a supported platform (A18) · Multi-machine frame-locked sync and output nodes (D11) · Web Player (WASM/WebGPU, D15) · XPBD cloth/hair and SPH/PBF fluids · VOP-style visual Kernel editor · FFGL hosting · real-time collaborative sessions · Slang compiler path · Houdini Engine bridge · more video models as they become consumer-feasible.

---

## 10. Licensing and compliance
- Core: MIT OR Apache-2.0 (A4). Contributions under DCO.
- Out-of-process only: ComfyUI (GPL-3.0). Optional plugin with its own licence: Ableton Link (GPLv2+). Dynamic load only: NDI (royalty-free SDK; follow naming/attribution rules).
- No model weights in the repo or installers. Model manager records licence and source per model.
- Clean-room AI implementation: StreamDiffusion techniques from the paper and Apache-2.0 repos (cumulo-autumn, daydreamlive); no code from closed or Patreon-gated tools.
- Trademarks: no use of "TouchDesigner", "Resolume", "Houdini" or their operator family names in product UI; comparisons only in docs.

---

## 11. Success metrics (12 months after v1.0)
- Installed on all NCAD media-lab images; used in ≥2 taught modules across Fine Art Media, Product Design (Interaction) and Communication Design.
- ≥1 public live performance and ≥1 installation delivered on Atidarla.
- ≥500 GitHub stars; ≥25 external contributors; ≥50 community Packs.
- Agent eval ≥90% valid networks, re-run each release.
- Zero show-stopping crash reports in the Stage path for two consecutive minor releases.

---

## 12. Open questions (resolve during M0)
1. Ratify the remaining Proposed decisions (A5, A8, A9, A11–A14).
2. Confirm PT-3 after the RTX 4070 benchmark.
3. Which cloud providers to support first (proposal: Decart + self-hosted Runpod container; Daydream once its API terms and Scope licence are clear).
4. Should the NCAD AI estate (Meitheal orchestrator) be the reference LAN host implementation?
5. Company GitHub org name and repo slug.
