# Atidarla — RESEARCH.md

**Version:** 1.0 · **Date:** 15 October 2026 · **Author:** Mark (with Claude)
**Status:** Research complete; feeds `PRD.md` v1.0
**Supersedes:** NascD RESEARCH.md / PRD.md v0.4 (11 July 2026). NascD decisions are carried forward where still valid and explicitly revised where the October 2026 evidence changed (see §9).
**Licence intent:** open source (MIT OR Apache-2.0), released under Mark's company on public GitHub; NCAD is first adopter.
**Platform scope (ratified 15 Oct 2026):** Windows and macOS are the targets; Linux is nice-to-have. Platform-exclusive capabilities (e.g. NVIDIA real-time diffusion on Windows, Kinect) are allowed when flagged. The Linux evidence below is kept for completeness.

**Confidence legend:** facts below were verified on vendor pages, release notes, repos or papers during this research pass unless marked **[U]** (unverified / secondary source / estimate). Prices change; re-check before quoting externally.

---

## 1. Executive summary

1. **The market is split three ways and nobody joins the pieces.** Performance tools (Resolume, VDMX, MadMapper, Millumin) own the show; node engines (TouchDesigner, vvvv, TiXL, Notch) own the patch; Houdini owns procedural geometry and simulation. Resolume itself splits patching into a separate product (Wire, 2D only). No cross-platform, open-source tool combines a TD-class node engine, a Resolume-class performance layer and Houdini-style procedural geometry. ossia score (GPLv3, Win/Mac/Linux) and TiXL (MIT, Windows) are the nearest, and each covers only part.
2. **Linux is the clearest unmet platform need** (noted for completeness; Mark has scoped Linux as nice-to-have, so it is not used as a differentiator). TouchDesigner has had Linux requests on its forum since 2011 with no staff commitment; the only route in 2026 is a community Wine installer (NVIDIA-dependent operators broken). Resolume, VDMX, Millumin, Notch, Smode, vvvv and TiXL all lack native Linux.
3. **AI has arrived, but only at the edges.** Resolume 7.26 added MCP servers (build-time only, by Resolume's own description) and 7.28 added segmentation/depth nodes; MadMapper has prompt-to-material ("MadAI"); TouchDesigner has no first-party AI — everything is third party (dotsimulate's StreamDiffusionTD and LOPs, a fragmented set of MCP servers). Live diffusion is bolted on through Spout/NDI from separate apps (Daydream Scope). **No mainstream product treats generative AI as a first-class, tempo-synced, performable layer.**
4. **Real-time diffusion is real on NVIDIA and still not elsewhere.** SD-Turbo with StreamDiffusion techniques reaches ~94–106 fps on an RTX 4090 (author numbers) and ~24 fps on a 4070 laptop. No published Apple Silicon or AMD numbers reach real time. Streaming *video* models (StreamDiffusionV2, Self-Forcing, LongLive, Krea Realtime) are Apache-2.0 but need 18–28 GB VRAM or cloud GPUs. Cloud real-time (Decart, Daydream, self-hosted on Runpod) fills the gap for Mac/AMD users at a per-second cost.
5. **Houdini 22 (July 2026) validates the direction.** Copernicus (GPU image network) now carries ONNX inference, SAM2 masks, MoGe-2 depth and neural cellular automata — Houdini is converging on "GPU image graph + ML nodes", i.e. TouchDesigner TOPs with AI. Its attribute model, Wrangle/VEX, HDAs, compiled blocks and PDG task graphs are the most valuable concepts to port; heavy solvers (FLIP, sparse pyro, RBD) are not real-time and should be imported as bakes (VAT).
6. **TouchDesigner's 2025–26 POPs confirm the GPU-resident point model** NascD already planned — but POPs are closed. Atidarla's equivalent should follow Houdini's attribute model more closely than TD's.
7. **The July stack holds, with three corrections:** keep Rust + wgpu (v30) + egui (0.37) + PyO3 (0.30); but (a) run all torch/TensorRT/ComfyUI workloads in **supervised out-of-process AI hosts**, (b) do not depend on free-threaded Python, (c) budget for a hand-written node canvas and a thin per-OS GPU-interop crate on `wgpu-hal`.
8. **Licensing shapes architecture.** ComfyUI (GPL-3.0) and Ableton Link (GPLv2+ or proprietary) must stay out-of-process or optional plugins to keep an Apache-2.0 core. Many fast models are non-commercial (SD-Turbo, SDXL-Turbo, FLUX dev, Depth Anything 3 large variants, Scope per its GitHub licence). Models must be user-downloaded with visible licence badges.

---

## 2. Market landscape

### 2.1 Performance / VJ / media-server tools

#### Resolume (Avenue / Arena / Wire) — the reference performance layer

| Item | Detail |
|---|---|
| Version | 7.28.1 (6 Oct 2026); 7.29 in preview (audio engine/FFT, slice effects) |
| Price | Avenue €299, Arena €799, Wire €399; perpetual + 12 months updates; renewals €79 / €219 / €119 per year; 50% education discount |
| OS | Windows 10+, macOS 10.15+ (Apple Silicon). **No Linux** (users run Avenue under Wine; only DXV plays) |
| Core model | Composition → decks → layers × columns of clips; per-clip/layer/comp effects; crossfader; groups; BPM/tap; Ableton Link (hidden by default) |
| Arena extras | Advanced Output (slices, masks, projector blending, warping, LED mapping), DMX via Art-Net (Lumiverses, fixture editor, pixel mapping), SMPTE timecode |
| Codec | DXV (proprietary GPU codec); staff recommend converting to DXV |
| Wire | Separate node patcher for building effects/sources/mixers; ISF support with in-app editor; **2D only** |
| AI (2025–26) | 7.26: MCP servers for Arena/Avenue and Wire (natural-language comp/patch building; Resolume positions them for building, not live performance; Wire MCP writes ISF). 7.28: Human Segmentation and Depth Estimation Wire nodes, Background Removal and Depth effects, Depth blend mode |
| Pain points | Development effort seen as going to Wire; requests for autopilot ("play next column"), internal timecode across layers, full show export, 3D; freezes reported on Apple Silicon with capture devices and on a Threadripper rig fed by TD over Spout; renewal model confuses buyers |

#### Others

| Product | OS | Price (2026) | Standout features | Limitations |
|---|---|---|---|---|
| **VDMX6 / Plus** | macOS 12+ | $199 / $349 | Deep modular UI, Metal, HAP R, tempo tracker, ISF/FFGL/Vuo sources, Plus hosts TD and Vuo compositions | Mac only |
| **MadMapper 6** | Win, Mac | Free (watermark), €39/mo rental, perpetual from €399 (per major version) | Mapping to 16K, LED/DMX pixel mapping, 3D calibration, space/LED scanning, cueing, live shaders, timelines, MadLaser, **MadAI** prompt-to-material (credits) | No node patching; perpetual covers one major |
| **MiniMad** | Raspberry Pi box | €229 | Cheap 1080p playback node, 64 universes | No 3D surfaces |
| **Millumin 5** | macOS 11+ | €29/7d … €199/yr; lifetime from €399 | Timeline/sequencer for theatre, JS scripting, timecode, Kinect, ILDA lasers, loads TD `.tox` | Mac only (player node beta on Win) |
| **Synesthesia** | Win, Mac | $199 / $399 Pro | ~80 audio-reactive scenes, Shadertoy/ISF import, live-coding, marketplace | No mapping, no clip deck |
| **Smode** | Windows | Community free; others by quote [U] | High-end real-time 2D/3D compositing, mapping, Notch/TD/Unreal integration, XR | Windows only, dongles |
| **Notch** | Windows | Builder $279–2,589/yr; playback $98/week or $899–7,535 perpetual | Real-time 3D VFX nodes; Notch Blocks play in media servers and TD | Windows only, subscription |
| **HeavyM** | Win, Mac | tiers [U] | Easy mapping, music reactivity, **built-in AI image/video generation** | Shallow |
| **Modul8** | macOS | [U] | Legacy, Rosetta-only on Apple Silicon | Effectively unmaintained |
| **Lumen** | macOS | [U] | Semi-modular analog-style video synth | Mac only |
| **disguise Designer** | GX servers | enterprise | Touring/broadcast standard (r32–r34) | Hardware-bound |

#### AI-native newcomers

- **Daydream Scope** (Livepeer/Daydream): local-first real-time video/audio engine — node builder, plugins, OSC/MIDI/Ableton Link/NDI/Spout/Syphon (per its own docs; an earlier source said OSC/MIDI were absent — features are moving fast), models StreamDiffusionV2, LongLive, Krea Realtime, RewardForcing, MemFlow, LTX-2 (plugin), Waypoint-1; v0.2.x; Windows and Mac, Linux "coming soon" [U]; Livepeer network cloud backend with billing. **Licence conflict:** GitHub `LICENSE.md` is CC BY-NC-SA 4.0 (non-commercial); the Hugging Face card says Apache-2.0. Treat as non-commercial until resolved.
- **dotsimulate (Lyell Hintz)** — StreamDiffusionTD v0.4.1 (Patreon-distributed): Windows + NVIDIA locally (Python 3.11.9, CUDA 12.8, TensorRT 10.16, torch 2.11), SDXL-Turbo, multi-ControlNet, IP-Adapter/FaceID, StreamV2V cached attention, live LoRA weights, FX feedback, GPU-resident frame transport; **Daydream Cloud mode** for any OS including Mac. Also **LOPs** (60+ language operators for LLM/RAG/tool-calling in TD). Licence/pricing not public [U].
- Browser and lightweight AI-VJ tools (freebeat, VJ Studio, Karleido etc.) are marketing-heavy with multi-second latency [U].
- **Pattern:** AI arrives as (a) vision ML nodes, (b) prompt-to-asset generators, (c) standalone diffusion apps piped in via Spout/NDI, (d) build-time MCP agents. **No one offers live diffusion as a tempo-locked, MIDI-mapped clip/layer, and no one offers in-show agents with safety rails.**

### 2.2 Node-based real-time environments

#### TouchDesigner (Derivative) — the reference node engine

| Item | Detail |
|---|---|
| Build | Official 2025.33230 (1 Sep 2026); "2025" series still current |
| OS | Windows 10/11 (Vulkan 1.1), macOS 13+ (Vulkan via MoltenVK — no geometry shaders; Face Track, Notch TOP, Kinect, OpenVR, Video Stream Out unavailable on Mac). **No Linux** |
| Python | 3.11 (3.11.15) |
| Licences | Non-Commercial free (1280×1280 cap, non-transferable keys); Educational $300 (approval, non-paying work); Commercial $600; Pro $2,200 (Sync CHOPs, frame-lock, projector calibration, camera tracking, RenderStream, private .toe, unlimited Notch); TouchPlayer $300/$800; Floating Cloud $900/$3,300 |
| Cook model | Pull-based, lazy: outputs request data each frame, upstream nodes cook only when requested and dirty; main thread runs UI, timeline and cooking; Python threads cannot touch TD objects (Thread Manager since 2023) |
| POPs | Official since 2025.31550 (30 Oct 2025). GPU-resident points + attributes; render via Render TOP or out to DMX/LED/lasers. 2026 additions: Text, Trace, Triangulate, Alembic Out (Mar); glTF In/Out, Script POP, `.pop` format, quaternions (Jul); Time Filter, Point File In with Gaussian-splat auto-detect, closed-volume groups (Sep) |
| AI | None first-party. Third party: StreamDiffusionTD, LOPs, YOLO (Patreon), Embody (free, text "TDN" diffable format, Claude Code), touchdesigner-mcp (MIT; exposes unsandboxed `execute_python_script`), TwoZero, tdmcp, TDPilot |
| Pain points | No Linux; Mac fidelity gaps; binary `.toe` defeats Git (community workarounds export DATs to text); main-thread limits; licence friction (node-locked keys, dongles, cloud-licence issues); MCP fragmentation and unsafe code-exec tools |

**Operator families — what Atidarla must replicate:**

| TD family | Purpose | Replicate in Atidarla as |
|---|---|---|
| TOP | GPU texture/image processing (Render, Composite, GLSL, Movie File In/Out, Layer Mix, NDI/Spout/Syphon) | **IMG** family on a Copernicus-style typed-layer model (§2.3) |
| CHOP | Time-sliced channels: control, audio, MIDI/OSC; exports to parameters | **SIG** family with shared timebase, sample-rate-aware, exportable to any parameter (Bitwig "modulate anything" ergonomics) |
| SOP | CPU geometry / modelling | **MSH** family (topology ops on CPU: boolean, extrude, subdivide, polyreduce, UV) — shares the GEO wire with PNT, no conversion nodes |
| POP | GPU points/attributes/simulation | **PNT** family on Houdini's attribute model (point/prim/detail + groups) |
| DAT | Tables, text, scripts, callbacks, web server | **DAT** family (tables/JSON/text/script) |
| MAT | Materials/shaders | **MAT** family (PBR, unlit, custom WGSL) |
| COMP | Containers, UI panels, 3D scene objects, extensions | **CMP** containers + **Packs** (HDA-style, §2.3) |

Cross-cutting ideas to keep: lazy pull cook, parameter expressions and exports, component extensions, self-contained components (`.tox` → Packs), a Palette of ready-made components.

#### Other node environments

| Product | Engine / OS | Licence / price | Ideas worth stealing | Limits |
|---|---|---|---|---|
| **vvvv gamma** 7.x | C#/.NET 8, VL.Stride; Windows only | Free non-commercial; paid commercial per developer | State hot-reload; NuGet extension model; **boygrouping** (still unreleased — planned for 8.0) | Windows only |
| **TiXL (Tooll 4)** 4.1 | C#/.NET, DX11; Windows (Wine elsewhere) | MIT | Timeline + node graph integrated; compute shaders; **Skill Quest** interactive learning (LGM 2026) | Windows-only; reviewers call docs thin and packaging weak |
| **cables.gl** | Browser WebGL; standalone app | Open source (licence unclear) [U] | Tiny exports with only used ops; in-editor op code editing; embeddable | Web-first |
| **ossia score** 3.8.2 | C++; Win/Mac/Linux/WASM/RPi | GPLv3 | Interactive timeline with triggers; OSCQuery/MQTT/CoAP/Art-Net; ISF/Faust/Pd/JS scripting | GPL, sequencing focus |
| **Max 9 / Jitter** | Win, Mac | $399 perpetual / $120 yr | Gen code generation; JS V8 | Closed, Jitter GL-era |
| **Pure Data / Gem** | All OS | Open source | Tiny, embeddable (libpd) | Dated GL/UI |
| **Unreal Motion Design + Niagara** | Win/Mac/Linux | Unreal EULA | Cloners/effectors (Cinema 4D MoGraph-style); Niagara modules + GPU sim | Heavy, not a performance tool |
| **Unity VFX Graph** | — | Unity licence | Reusable event-driven graph assets; sub-graphs | Engine-bound |
| **Blender Geometry Nodes** 5.x | All OS | GPLv3 | Fields; simulation/repeat/for-each **zones** (explicit loop state); bundles & closures (5.0); volume grids | Not real-time |
| **Hydra** | Browser | AGPL-3.0 | Chainable live-coding syntax; WebRTC stream sharing between performers | Code only |
| **Bitwig Grid** | — | Bitwig | Any output to any input; modulators on every parameter; polyphonic signals | Audio |
| **ISF / Shadertoy** | Spec | ISF spec open | JSON-header shaders auto-generate UI; huge shader corpus | GLSL — needs translation to WGSL |

### 2.3 SideFX Houdini — concepts to port

| Item | Detail |
|---|---|
| Version | **Houdini 22** (16 Jul 2026). H21 (Aug 2025) added Copernicus Flow, COP Pyro (GPU sparse), OTiS GPU muscle, KineFX autorig, Neural Point Surface |
| H22 headlines | Full Gaussian-splat toolset (train, animate, relight, Karma render); Copernicus: terrain, ocean, Ripple solver, time nodes, **Neural Cellular Automata, SAM2 masks, MoGe-2 depth, ONNX inference**; Houdini Engine for Unreal runs Copernicus HDAs; Python 3.13, Qt 6.8; Intel Mac dropped; Linux arm64 preview; MCP-driven APEX rigging shown at SIGGRAPH 2026 as a Labs preview |
| Licences | Apprentice free (NC); Indie ~$299/yr (<$100k revenue); Core ~$1,995 perpetual / ~$1,475 yr; FX ~$4,495 / ~$3,505 yr; Education ~$95; Engine for Unreal/Unity free [U — secondary sources] |
| OS | Windows, macOS (Apple Silicon only), Linux |

**Concept map (Houdini → TD → feasibility at 60 fps → Atidarla decision):**

| Houdini concept | TD equivalent | RT feasibility | Atidarla decision |
|---|---|---|---|
| Attribute model (point/vertex/prim/detail + groups) | POP attributes | High (SoA GPU buffers) | **v1 core** of the shared GEO type (PNT + MSH); vertex attributes deferred |
| SOPs | SOP/POP | Medium (GPU point ops) / CPU (topology) | Split: GPU point ops in **PNT**; topology/modelling ops (boolean, extrude, bevel, subdivide, polyreduce, remesh, sweep, UV) in **MSH** on CPU with automatic GPU upload |
| VEX / Wrangle | GLSL POP/TOP, Script POP | High | **v1 key feature: Kernel DSL** (§5.4) running over points, prims, pixels, voxels |
| VOPs | — | High | v1.5 visual editor over the kernel IR |
| **Copernicus (COPs)** | TOPs | High | **v1 core of IMG family**: typed layer signatures (Mono/UV/RGB/RGBA/ID), border modes, metadata layer, tile size, bundled "cables" |
| COP solvers (reaction-diffusion, flow, ripple, NCA) | Feedback TOP loops | High | v1 as **Solver Block** nodes |
| COP Pyro (sparse 3D) | — | Medium | Deferred; 2D/dense small grids only |
| DOPs framework | — | varies | Not cloned — **Solver Block** with explicit carried state (Blender zone pattern) |
| CHOPs | CHOPs | High | SIG family |
| LOPs / Solaris (USD) | — | n/a | USD/glTF import only |
| **TOPs / PDG** task graphs | none | offline | **v1.5 differentiator: Batch Graph** — wedge prompts/seeds/LoRAs across ComfyUI/cloud jobs; results cached by hash and land in the clip bin |
| APEX / KineFX | limited | Medium | Deferred; glTF skinning + VAT |
| **HDAs** | COMP / `.tox` | High | **v1 Packs**: promoted parameter UI, semver, embedded help, nested |
| Houdini Engine | — | Medium | Pack Player runtime (headless/kiosk/web) |
| Cook model (lazy pull; "compiled" COP sections) | lazy pull | High | Dirty-flag DAG compiled to a per-frame render graph; **Compiled Block** fuses kernels |
| Compiled blocks / For-Each | Replicator | Med-High | v1 For-Each over points/instances compiled to one dispatch |
| ML (ONNX SOP/COP, SAM2, MoGe-2, NCA) | community | Medium | **v1 ONNX Inference node** on IMG and GEO |
| Vellum (XPBD) | POP forces | Med-High | v1.5 GPU XPBD cloth/rope/hair (graph colouring or Jacobi) |
| FLIP / MPM / RBD | — | Low–Med | Deferred; SPH/PBF fluid v1.5; import RBD/fracture as **VAT 3.0** bakes |
| Gaussian splats | Point File In POP | High (render) | v1.5 splat render node (PLY/SPZ/SOG); training stays offline |

**Open-source analogues worth studying:** Blender Geometry Nodes zones (loop-state model), NVIDIA Warp 1.18 (Apache-2.0; CUDA/CPU only — reference, not runtime), Taichi (stagnant since 1.7.4, Jul 2025), Genesis (Apache-2.0 MPM/SPH/PBD on GPU), EmberGen (quality bar for GPU smoke; 2.0 due 2026), Gaea 2.3 (macro system as HDA analogue).

### 2.4 Market comparison matrix

| Product | OS | Price | Open source | Node engine | Clip launcher | Timeline | 3D/procedural geo | Mapping | DMX | Live AI diffusion | Agent/MCP | Multi-machine |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Resolume Arena | W M | €799 | – | Wire (2D, separate app) | ✔ | cue | – | ✔ | Art-Net | – (CV nodes only) | build-time | timecode |
| TouchDesigner | W M | free NC / $300–2,200 | – | ✔ | DIY | basic | POPs (closed) | DIY / Pro | ✔ | third-party | third-party | Pro only |
| Houdini | W M L | free NC / ~$299–4,495 | – | ✔ | – | ✔ | ✔✔ | – | – | – | Labs preview | farm |
| vvvv gamma | W | free NC / paid | partial [U] | ✔ | – | – | ✔ | DIY | ✔ | via Python | – | planned |
| TiXL | W | free | MIT | ✔ | – | ✔ | partial | – | – | – | – | – |
| ossia score | W M L | free | GPLv3 | partial | – | ✔✔ | partial | – | ✔ | – | – | – |
| Notch | W | $279–2,589/yr | – | ✔ | – | ✔ | ✔ | – | – | – | – | render nodes |
| MadMapper | W M | €399+ | – | – | cue | ✔ | – | ✔✔ | ✔ | prompt-to-material | – | MiniMad |
| Daydream Scope | W M (L soon) | free / cloud billing | NC licence [U] | pipeline | – | ✔ | – | – | – | ✔ | – | – |
| **Atidarla (target)** | **W M** (L best-effort) | **free** | **Apache-2.0** | **✔** | **✔** | **✔** | **✔** | **✔** | **✔** | **✔ (first-class)** | **✔ (show-safe)** | **✔ (free)** |

---

## 3. Feature subsets to adopt

These are the subsets Atidarla takes from each reference. Detail and phasing live in `PRD.md`.

### 3.1 From Resolume (performance layer — "Stage")
- Composition → decks → layers × columns; clip triggering (launch/legato/retrigger/one-shot), column launch, crossfader A/B, layer groups.
- Clip types: media file, live input, **generator/patch** (any node network or Pack), **AI clip** (prompt + model + params; see §7 D3).
- BPM: tap, Ableton Link (optional plugin), audio beat detection; beat-synced parameter automation and transitions.
- Effects chains at clip/layer/group/comp level with blend modes; per-clip transitions.
- Advanced Output: slices, masks, warping (bezier/grid), edge blending, LED pixel mapping; 10-bit and HDR output.
- DMX/Art-Net **and sACN** out, fixture profiles, pixel mapping.
- MIDI/OSC learn with live monitors; REST/OSC API.
- Autopilot (play next/random/specific column) and timecode chase — both forum-requested gaps.
- Full show export (single portable bundle) — forum-requested gap.

### 3.2 From TouchDesigner (node engine)
- Operator families (§2.2 table), pull-based lazy cook, parameter expressions, channel exports, networks-in-networks, component extensions, Palette.
- Python scripting with a TD-familiar API shape (`op('/proj/noise1').par.period = 2`).
- Panel/UI-building components for installations (buttons, sliders, layouts) and **perform mode** (UI-only window).
- Viewers on every node (texture, channel, geometry, table).

### 3.3 From Houdini (procedural core)
- Attribute model, Kernel DSL (VEX-like), Packs (HDA-like), compiled blocks/for-each, Copernicus-style typed image layers, 2D solvers, ONNX nodes, PDG-style Batch Graph, VAT import. (§2.3)

### 3.4 From the rest
- **vvvv:** state-preserving hot reload of kernels and Python operators; package-manager-based extensions.
- **TiXL:** integrated keyframe timeline; Skill Quest-style learning tours.
- **ossia score:** interactive timeline with triggers/conditions; OSCQuery.
- **cables.gl / Hydra:** minimal web export; WebRTC stream sharing for collaborative performance.
- **Bitwig Grid:** modulators attachable to any parameter; any-to-any signal ergonomics.
- **ISF/Shadertoy:** import and auto-UI; transpile GLSL→WGSL via `naga`.
- **Blender:** zone semantics for solver state; geometry-node "fields" mental model.
- **Embody (TD community):** text, diffable project format usable by agents.

---

## 4. AI landscape (October 2026)

### 4.1 Real-time image diffusion

**StreamDiffusion (cumulo-autumn, Apache-2.0)** — the technique set that makes live diffusion work: Stream Batch (denoising steps batched across frames), Residual CFG, Stochastic Similarity Filter (skip near-identical frames), TAESD tiny VAE, TensorRT, IO queues, pre-computed prompt embeddings. Author benchmark on RTX 4090: SD-Turbo 1-step 106 fps txt2img / 94 fps img2img; LCM-LoRA 4-step ~38 fps. **The original repo is dormant** (last PyPI release Dec 2023).

**Daydream fork (daydreamlive/StreamDiffusion, Apache-2.0)** — the active line: SDXL (taesdxl + TensorRT), runtime multi-ControlNet, IP-Adapter, FaceID, TemporalNet, StreamV2V; torch 2.7–2.8, CUDA 11.8/12.8/12.9. Daydream claims 15–25 fps with complex ControlNet/IP-Adapter configs (GPU unnamed).

**dotsimulate StreamDiffusionTD v0.4.1** — the de facto artist-facing implementation (see §2.1). Key lessons to adopt: GPU-resident frame transport between host and Python, live LoRA weighting, feedback FX processors, cloud fallback for Mac.

**Measured SD-Turbo 512² fps (1-step):**

| GPU | fps | Source |
|---|---|---|
| RTX 5090 | "well over 100" (SDXL-Turbo 1024²: ~23) | community (vvvv forum) |
| RTX 4090 | 94–106 (raw); 55–60 inside TouchDesigner | StreamDiffusion README; TouchDiffusion |
| RTX 4080 | 47 | TouchDiffusion |
| RTX 3090 | 30–32 | TouchDiffusion |
| RTX 4070 laptop | 24 | TouchDiffusion |
| RTX 4070 desktop (NCAD lab baseline) | ~30–40 **[U — estimate; benchmark in M0]** | interpolation |
| RTX 3060 12 GB | 16 | TouchDiffusion |
| Apple M4/M5 Max | no published figure; expect single digits–mid-teens **[U]** | — |
| AMD RX 7900 XTX / 9070 | no published figure; expect ~10–25 **[U]** | — |

### 4.2 Streaming video models (all Apache-2.0 unless noted)

| Model | Base | Reported speed | Practical role |
|---|---|---|---|
| StreamDiffusionV2 (MLSys 2026 best paper) | Wan2.1 1.3B/14B | 58–65 fps on 4×H100; ~0.5 s first frame; ~18 GB VRAM single GPU (third-party) | Cloud / high-end local |
| Self-Forcing | Wan2.1 1.3B | 16 fps H100; ~10 fps RTX 4090 | Near-real-time local on 24 GB |
| LongLive (2.0) | Wan2.2 | 24.8 fps BF16; 45.7 fps NVFP4 2-step (GPU unnamed); 20.7 fps H100 (1.0) | Near-real-time local / cloud |
| Krea Realtime 14B | Wan2.1 14B | 11 fps on B200 | Cloud |
| SANA-Streaming 2B | — | 24 fps at 1280×704 on RTX 5090 | **Watch** — weights/licence pending [U] |
| Decart Lucy 2.5 / Restyle 2 | proprietary | <40 ms/frame claimed; $0.02 / $0.01 per second | Cloud live restyle |

### 4.3 Near-real-time and offline generation
- **Fast text-to-image:** SANA-Sprint 1.6B (~0.3 s at 1024² on RTX 4090; card says research use), FLUX.1 schnell and FLUX.2 klein 4B (Apache-2.0; seconds per image), Hyper-SD/LCM LoRAs.
- **Offline video / 3D:** Wan 2.2 (5 s of 720p ≈ 9 min on 4090), TRELLIS.2-4B (MIT, ~3 s on H100, 24 GB+), Hunyuan3D 2.1 (custom licence).

### 4.4 ComfyUI (v0.40.0, 13 Oct 2026; GPL-3.0)
- HTTP (`POST /prompt`, `/queue`, `/interrupt`, `/history`, `/view`, `/upload/image`, `/object_info`, Jobs API) + WebSocket `/ws` progress events; OpenAPI 3.1 spec since v0.20.
- Two JSON formats: UI workflow (nodes/links/groups) vs **API format** (what `/prompt` accepts). `/object_info` exposes every node's input schema — ideal for auto-generating Atidarla parameter UIs.
- Subgraphs official since Aug 2025; Manager + custom-node ecosystem; partner/API nodes for closed models billed in Comfy credits (`--disable-partner-nodes`, `--offline` flags).
- Comfy Cloud: $20 / $35 / $100 / $700 per month; queue-based, not low latency.
- Integration precedent: Krita AI Diffusion (Live Painting over a local/remote ComfyUI backend), ComfyUI-TD nodes, Blender add-ons; **ComfyStream** (Livepeer) streams video frames through a ComfyUI graph over WebRTC.
- **Implication:** integrate as an external process over HTTP/WS — never link or bundle — to keep the core permissively licensed.

### 4.5 Vision, audio and language models (preprocessors and controllers)

| Model | Task | Licence | Notes |
|---|---|---|---|
| Depth Anything 3 | depth | Apache-2.0 (Small/Base/Metric-L/Mono-L); **CC BY-NC** (Large/Giant/Nested) | Streaming variant <12 GB |
| Video Depth Anything | temporally stable depth | check per model | |
| SAM 3 / 3.1 | segmentation, tracking | custom SAM licence | Python 3.12+, CUDA 12.6+ |
| MediaPipe | pose/hands/face | Apache-2.0 | CPU and GPU on all OS — the universal fallback |
| RTMPose, RAFT | pose, optical flow | [U] | |
| Whisper (turbo) | speech-to-text | MIT | ~6 GB |
| Demucs | stems | MIT, **archived Jan 2025** | |
| Magenta RealTime 2 | live music generation | Apache-2.0 | real time on Apple Silicon only |
| Stable Audio Open | audio gen | Stability community | |
| Ollama / llama.cpp | LLMs | MIT | CUDA, HIP, Metal, Vulkan, SYCL backends |

### 4.6 Inference runtimes by vendor

| Vendor / OS | Fastest path | Status |
|---|---|---|
| NVIDIA Win/Linux | TensorRT 10.x / **TensorRT for RTX** (JIT engines, portable, <200 MB) | Real-time verified |
| Windows (any GPU) | **Windows ML** (ONNX Runtime; EPs: NvTensorRtRtx, MIGraphX, OpenVINO, QNN, WebGPU experimental) | DirectML is legacy/"sustained engineering" |
| AMD Windows | ROCm 7.2.x "PyTorch on Windows" (Win 11; RX 7900/9070/9060, R9700); full ROCm stack not on Windows | Interactive tier |
| AMD Linux | ROCm / MIGraphX | Interactive tier |
| Apple | Core ML (ANE), MLX, MPS | Interactive tier; StreamDiffusion-Mac fork exists (no fps) |
| Any (Vulkan) | stable-diffusion.cpp (MIT; CUDA/Vulkan/Metal/SYCL) | Portable fallback |
| Intel | OpenVINO | Interactive tier |

### 4.7 Cloud providers

| Provider | Mode | Pricing signal |
|---|---|---|
| Decart | WebRTC streaming V2V | $0.01–0.04 / s |
| Daydream / Livepeer | WebRTC StreamDiffusion SDXL, Scope cloud | not public [U] |
| fal.ai | WebSocket real-time (msgpack), sub-100 ms claim | endpoints shrinking — verify |
| Runpod serverless | self-host any container (e.g. StreamDiffusionV2 / LongLive) | RTX 4090 $1.10/h … H100 $4.79/h |
| Replicate | HTTP/queue | per-second GPU billing |
| Runware, Krea API | async jobs | per image/video |
| Comfy Cloud | queue | subscriptions |

**Also relevant for NCAD:** the in-house AI estate (2× DGX Spark, 6-GPU Threadripper, RTX 4090, lab RTX 4070s) and the "Meitheal" orchestrator project mean a **LAN inference tier** — remote AI hosts on the institution's own hardware — is both a sovereignty win and a product feature any studio or school could use.

### 4.8 GPU interop for AI frames
- Zero-copy is realistic on **NVIDIA + Windows/Linux only** (CUDA–Vulkan/D3D12 external memory; CUDA IPC across processes, as cuda-link does for TD). `wgpu-hal` now exposes Vulkan dmabuf import, external semaphores and `buffer_from_raw`; safe-API external textures are DX12-only.
- On Mac and AMD plan **one readback per frame** (cheap at 512²) or IOSurface sharing **[U — spike]**.
- DLPack exchanges tensors between frameworks; it does not by itself share graphics textures.

### 4.9 Model licensing for an open-source app
- **Commercial-safe:** FLUX.1 schnell, FLUX.2 klein 4B, Wan 2.x, StreamDiffusionV2, Self-Forcing, LongLive, Krea Realtime, MediaPipe, Whisper, Depth Anything 3 small variants, TRELLIS.2 (MIT).
- **Use-restricted (OpenRAIL):** SD 1.5, SDXL base.
- **Non-commercial or gated:** SD-Turbo / SDXL-Turbo (Stability community/NC), SD3.5 (free under $1M revenue), FLUX dev family, Depth Anything 3 large variants, Scope (per GitHub licence), Hunyuan3D (custom).
- **Policy:** never bundle weights; download on demand after the user accepts the model's licence; show a licence badge on every AI node; offer a "commercial-safe only" project filter.

---

## 5. Technology and stack validation

### 5.1 Stack (July → October 2026)

| Component | NascD choice | Evidence (Oct 2026) | Atidarla decision |
|---|---|---|---|
| GPU API | wgpu | v30.0.1 (Aug 2026); compute, storage buffers, subgroups, 64-bit atomics (Metal Apple9+), timestamp queries, binding arrays, HDR colour spaces; mesh shaders and ray tracing still experimental; ~quarterly breaking majors | **Keep**; pin one major per release train; feature-gate experimental paths |
| Raw Vulkan + MoltenVK | — | TD's Mac limits come from MoltenVK | **Reject** — wgpu's native Metal backend is an advantage |
| Dawn / SDL3 GPU | — | viable but weaker Rust story | Fallback only |
| Shading | WGSL | WESL 0.6 (imports/modules); Slang under Khronos but WGSL/Metal targets experimental | **WGSL + WESL**; Kernel DSL lowers to WGSL; Slang evaluated later as offline compiler |
| UI | egui + custom canvas | egui 0.37 (15 Oct 2026, wgpu 30), egui_dock 0.21; node-graph crates small/stale; Rerun proves egui carries heavy docked viewers | **Keep egui**; own the node canvas; UI behind an internal trait |
| Python | PyO3, 3.11+ | PyO3 0.30.1 supports 3.15; free-threading opt-in from 3.14 but C-extension ecosystem not ready | **Keep**; ship 3.13 (GIL) default, floor 3.11, free-threaded as build option |
| AI runtime | in-process Python | torch in-process = signing, wheel size and crash risk during shows | **Change: supervised out-of-process AI Hosts** |
| Env manager | uv | Astral acquired by OpenAI (Mar 2026); tools remain permissive | **Keep uv**; mirror pinned python-build-standalone builds |
| macOS packaging | — | deep-sign all Mach-O, hardened runtime, `disable-library-validation` for third-party extensions | Day-one CI spike |
| Video | FFmpeg LGPL + HAP | `ffmpeg-next` 9.0; `gpu-video` 0.4 (Vulkan Video decode straight to `wgpu::Texture`, NVIDIA/AMD) | Keep; add Vulkan Video path; VideoToolbox on Mac |

### 5.2 Interop matrix

| Protocol / format | Windows | macOS | Linux | Licence / note |
|---|---|---|---|---|
| Spout | ✔ | – | – | BSD-2 |
| Syphon | – | ✔ | – | BSD-style [U]; Rust bindings to write |
| Linux texture sharing | – | – | libfunnel (PipeWire + DMA-BUF) | "Spout/Syphon for Linux"; maturity [U] |
| NDI 6 | ✔ | ✔ | ✔ [U] | Proprietary royalty-free SDK; dynamic load; naming/attribution rules; `grafton-ndi` crate |
| HAP / HAP-Q | ✔ | ✔ | ✔ | Open (Vidvox); GPU BC decode |
| DXV | ✔ | ✔ | [U] | Resolume proprietary — import only if FFmpeg supports [U] |
| Ableton Link | ✔ | ✔ | ✔ | **GPLv2+ or proprietary** → optional separately-licensed plugin |
| OSC / OSCQuery | ✔ | ✔ | ✔ | `rosc` (MIT); write own OSCQuery |
| MIDI 1.0 / 2.0 | ✔ | ✔ | ✔ | `midir`, `midi2` (MIT) |
| Art-Net / sACN | ✔ | ✔ | ✔ | `artnet_protocol`, `sacn` (MIT/Apache) |
| LTC / MTC | ✔ | ✔ | ✔ | write own LTC decoder |
| ISF | ✔ | ✔ | ✔ | own parser → WGSL |
| FFGL hosting | ✔ | ✔ | – | GL-centric ABI — deferred |
| Gaussian splats | ✔ | ✔ | ✔ | Brush (Rust/wgpu, Apache-2.0) reference; PLY / SPZ (~10× smaller) / SOG (~15–20×) / glTF `KHR_gaussian_splatting` |

### 5.3 GPU particles and simulation
- Pattern: GPU-resident structure-of-arrays buffers, indirect dispatch, subgroup reductions, spatial hashing for neighbours.
- Rust references: `bevy_hanabi` 0.19 (GPU particles), Brush (splats), WebGPU MLS-MPM demos.
- Python options (Warp, Taichi) are not cross-platform runtimes — at most an optional NVIDIA AI-Host plug-in.
- **Decision:** native wgpu compute solvers (particles, flocking, curl-noise advection, XPBD, SPH/PBF, 2D fluid, reaction-diffusion, NCA).

### 5.4 Kernel DSL ("VEX for the GPU")
- Recommended: a small, restricted per-element language with `@attr` bindings (`@P`, `@Cd`, `@v`, `@id`), `ch("param")` hooks and helpers (`point()`, `nearpoints()`, `noise()`), lowered through a typed IR to WGSL with bindings generated from the attribute schema. Every kernel compiles to one dispatch.
- Escape hatch: raw WGSL node with Houdini-OpenCL-style `#bind` declarations (read/write/optional), tile size, iteration count, and a "Generated Code" view.
- Rationale: Houdini itself pairs VEX Wrangles with a lower-level OpenCL node for performance; a DSL lets the compiler guarantee portability and safety (important for AI-generated kernels).

---

## 6. Market gaps (ranked by evidence)

1. **Native Linux and true cross-platform parity** — 15 years of TD forum requests; Wine workarounds in 2026; every performance tool is Win/Mac. *(Out of scope for v1 — Linux is nice-to-have.)*
2. **One environment for patch + perform + publish** — Resolume splits Wire from Arena; TD performance layers are DIY; nobody pairs a clip deck with 3D procedural geometry.
3. **Live AI as a performable layer** — today diffusion lives in separate apps over Spout/NDI; no tempo lock, MIDI mapping or clip semantics.
4. **Safe agent control** — fragmented TD MCP servers with unsandboxed code exec and hallucinated node types; Resolume's MCP is build-time only.
5. **Git-friendly, mergeable projects** — `.toe` is binary; collaboration guidance is "Dropbox or external `.tox`".
6. **Show reliability** — forum reports of freezes in both Resolume and TD; no tool isolates crash-prone AI workloads from the render loop.
7. **Licence cost and friction** — TD resolution cap, education approval, node-locked keys; Notch subscriptions; Houdini FX ~$4.5k.
8. **Mac graphics fidelity** — TD over MoltenVK lacks geometry shaders and several operators.
9. **Multi-machine sync is gated or missing** — TD Pro-only; vvvv boygrouping unreleased.
10. **Learning curve** — TiXL's Skill Quest and AI explainers point at demand for guided learning; strongest in education.
11. **Web delivery** — WebGPU makes a "publish to web" player plausible (degree shows, portfolios); demand unproven.

---

## 7. Proposed market differentiators

Each is tied to a gap above. Effort: S/M/L. All are proposals for Mark to ratify in the PRD decision log.

| # | Differentiator | Gap | What it means | Effort |
|---|---|---|---|---|
| **D1** | **Patch · Perform · Publish in one app** | 2 | Three synchronised views over one graph: *Patch* (node editor), *Stage* (Resolume-style clip deck + outputs), *Timeline* (keyframes + cues). Any network or Pack is a clip; any clip opens as a network. | L |
| **D2** | **Free, no caps, Windows + Mac parity** | 7, 8 | Windows and macOS (native Metal) at parity from M1; no resolution cap, no licence keys, no Pro tier — sync, mapping and output tools included. Linux best-effort. | M |
| **D3** | **AI clips & AI layers** | 3 | Diffusion/video models as clip types: prompt clips in the deck, prompt crossfades on the crossfader, seed-on-beat, ControlNet inputs from any IMG node, LoRA weights mappable to MIDI. An **Output Governor** keeps the show at 60 fps by decoupling AI frames (interpolate/hold/blend) when the model can't keep up. | L |
| **D4** | **Hybrid compute router with a visible meter** | 3, 7 | Each AI node routes to *Local GPU → LAN host → Cloud* by policy; badges show where it ran, latency, VRAM and cost; one switch for **Sovereign mode** (nothing leaves the machine/LAN). | M |
| **D5** | **Show-safe agent** | 4 | Built-in assistant + MCP server exposing *typed* graph operations (no arbitrary code exec by default); every agent edit is a reviewable diff with one-step undo; **Show Lock** limits agents during performance to whitelisted parameters and Packs. In-show voice/text cues ("go darker and slower on the next drop") resolve to parameter moves, not graph rewrites. | M |
| **D6** | **Git-native projects** | 5 | Project = folder of small text files (one per network/Pack), stable node IDs, deterministic ordering; built-in visual diff and three-way node merge. | M |
| **D7** | **Kernel DSL** | — | VEX-flavoured GPU kernels with auto-bindings; hot reload with state kept; AI can write kernels safely because the DSL constrains them. | M |
| **D8** | **Batch Graph (PDG for AI)** | 3 | Wedge prompts/seeds/LoRAs/ControlNet weights across local ComfyUI, LAN hosts or cloud; cache by hash; results land in the Stage clip bin with metadata. Turns show prep from hours into a sweep. | M |
| **D9** | **Bring-your-own ComfyUI** | 3 | Import an API-format ComfyUI workflow as a node; its inputs become parameters (from `/object_info`); run it against any ComfyUI server. Keeps GPL code out-of-process. | S |
| **D10** | **Crash-isolated show engine** | 6 | Render/output process separate from AI Hosts and Python; supervisor restarts a dead host in seconds while the output holds last frame; hot-standby output node; show recording for post-mortem. | M |
| **D11** | **Free multi-machine sync** | 9 | Frame-locked render clusters and output nodes (boygrouping-style) over LAN, plus timecode chase. | L (M7) |
| **D12** | **Capture-to-stage pipeline** | — | Gaussian splats (PLY/SPZ/SOG), Kinect/Orbbec/volumetric, VAT bakes and Houdini `.bgeo`-class point caches as first-class inputs — directly serving NCAD's photogrammetry rig and Kinect toolkit. | M |
| **D13** | **Learn mode** | 10 | Interactive quests inside the app; "explain this network" grounded in the operator docs; every operator ships with a runnable example. | S–M |
| **D14** | **Licence-aware models** | — | Licence badge per model, "commercial-safe" project filter, per-project model manifest for reproducibility. | S |
| **D15** | **Web Player** | 11 | Export a Pack or project subset to a WebGPU page (`wasm32`), cables-style "only what's used". | L (M7) |

**Recommended headline positioning:** *"The open visual instrument for Windows and Mac: patch it like TouchDesigner, play it like Resolume, grow it like Houdini — with AI you can perform."*

---

## 8. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Scope (three products in one) | High | High | Strict milestone gates; Stage view built on the same graph, not a second engine; ~200 core operators by v1.0, extensibility for the rest |
| wgpu breaking changes | High | Medium | Pin one major per release; internal GPU abstraction crate |
| Non-NVIDIA real-time AI below target | High | Medium | Accepted: local real-time is a Windows + NVIDIA capability; Mac uses LAN/cloud routing; Output Governor |
| GPU interop gaps (IOSurface, DX12 shared handles) | Medium | Medium | Spike in M0; readback fallback |
| Licence contamination (GPL/NC) | Medium | High | Process boundaries; plugin isolation; licence CI check; no bundled weights |
| macOS signing of Python/torch | Medium | Medium | AI Hosts as separately signed helper bundles; CI spike in M0 |
| Solo/small-team capacity | High | High | Claude Code-driven development with acceptance criteria; community contribution model; NCAD as test bed |
| Model churn | High | Low | Model adapters behind a stable AI-Host protocol |

---

## 9. What changed since NascD v0.4 (July 2026)

| NascD v0.4 | Atidarla v1.0 | Why |
|---|---|---|
| Name NascD, `.nascd` | **Atidarla**; `.atidarla` project folder; CLI/module `atidarla` | Mark, 15 Oct 2026 |
| Performance mode as a feature | **Stage view as a peer of Patch and Timeline** (D1) | Strongest market gap |
| AI runtime embedded via Python | **Out-of-process AI Hosts** (local, LAN, cloud) | Crash isolation, packaging, licensing |
| D5: real-time diffusion NVIDIA-only | **Tiered by capability**: local real-time is a Windows + NVIDIA platform feature (like Kinect); Mac/AMD interactive locally with LAN/cloud for real time | Cloud real-time now viable; ratified 15 Oct 2026 |
| Windows, macOS, Linux | **Windows + macOS; Linux nice-to-have** | Ratified 15 Oct 2026 |
| Ollama primary, Anthropic opt-in | Kept, plus provider interface for cloud image/video and LAN hosts; Sovereign mode | Hybrid requirement |
| MCP server | **Typed, permissioned MCP + Show Lock** | TD MCP safety lessons; Resolume MCP is build-time only |
| ~176 operators | **~200 by v1.0** in eight TD-mapped families: IMG/SIG/PNT/MSH/MAT/DAT/CMP/AI | Ratified 15 Oct 2026 |
| POP family (TD-modelled, 36 ops) | **PNT family (~38) on Houdini attribute model + Kernel DSL**, plus a new **MSH (SOP) family (~34)** sharing one GEO wire | SOP-class modelling was missing; ratified 15 Oct 2026 |
| Binary sidecar format | **Folder of text files**, binary only for caches | Git gap |
| Free-threaded Python implied | GIL Python default | Ecosystem readiness |
| Lab baseline RTX 4070 12 GB | Kept | Fleet unchanged |

---

## 10. Open questions to verify

1. RTX 4070 desktop SD-Turbo fps inside our pipeline (M0 benchmark).
2. Apple M-series and AMD RDNA3/4 SD-Turbo fps via Core ML/MLX and ROCm/Windows ML.
3. `wgpu-hal` import of DX12 shared handles and Metal IOSurfaces.
4. NDI SDK Linux support and current licence text.
5. Scope licence (CC BY-NC-SA vs Apache-2.0) and Daydream API pricing.
6. dotsimulate StreamDiffusionTD licence/terms (for clean-room boundaries — Atidarla must not copy its code).
7. Whether DXV is decodable via FFmpeg.
8. Slang WGSL/Metal targets leaving experimental status.
9. Houdini current prices (vendor page did not render).

---

## 11. Sources

**Resolume:** resolume.com/download · resolume.com/shop · resolume.com/shop/upgrade · resolume.com/software · resolume.com/support/en/tech-specs · resolume.com/article/27 (DMX) · resolume.com/article/63 (Link) · resolume.com/support/en/isf · resolume.com/blog/34484 (7.26 MCP) · resolume.com/blog/34630 (7.28 AI) · resolume.com/blog/33196 (survey) · forum threads p=80006, 97538, 90041, 85164, 98813, 94725, 105265

**Other performance tools:** vdmx.vidvox.net/blog/vdmx6 · madmapper.com/madmapper/software · shop.garagecube.com/products/minimad · millumin.com · synesthesia.live/pricing · smode.io/en/faq · notch.one/pricing · notch.one/pricing/playback-licensing · garagecube.com/modul8 · heavym.net/pricing · lumen-app.com · disguise r32 release notes · limeartgroup.com (VJ mega-list; future of VJing)

**TouchDesigner:** docs.derivative.ca/Release_Notes · derivative.ca/download · docs.derivative.ca/System_Requirements · docs.derivative.ca/Licensing · docs.derivative.ca/POP · docs.derivative.ca/Cook · docs.derivative.ca/Python_threading_in_TouchDesigner · derivative.ca/community-post/2025-official-update/73153 · derivative.ca/solr/vulkan · forum.derivative.ca Linux threads (2581; 11349; 978051) · derivative.ca/community-post/best-practices-touchdesigner-collaboration · brendandawes.com/blog/1279-touchdesigner-git · mcp.directory/blog/touchdesigner-mcp-complete-guide-2026 · interactiveimmersive.io (2025 release; 2026 community tools; AI tools 2026; POPs)

**Other node tools:** vvvv.org · thegraybook.vvvv.org/roadmap/planned.html · github.com/tixl3d/tixl · tixl.app · libregraphicsmeeting.org/2026 (Skill Quest) · cables.gl · github.com/ossia/score · cycling74.com/shop/max · puredata.info · github.com/hydra-synth/hydra · bitwig.com/the-grid · docs.isf.video · shadertoy.com/howto · developer.blender.org/docs/release_notes/5.0/geometry_nodes · docs.unity3d.com (VFX Graph 17) · unrealengine.com (Niagara 5.7)

**Houdini:** sidefx.com/products/whats-new-in-h22 · cgchannel.com/2026/07/sidefx-just-released-houdini-22 · cgchannel.com/2025/08 (H21) · sidefx.com/docs/houdini/copernicus/{intro,cooking,glossary}.html · sidefx.com/docs/houdini/nodes/cop/opencl.html · sidefx.com/docs/houdini/news/22/{ml,pdg,engine,platforms,vex,copernicus,pyro}.html · sidefx.com/docs/houdini/tops/intro.html · sidefx.com/docs/houdini/model/{compile,attributes}.html · sidefx.com/docs/houdini/vellum · superrendersfarm.com (Houdini prices) · jonpeddie.com (H22 MCP, SIGGRAPH 2026) · forum.babylonjs.com (VAT3 WGSL) · warp.readthedocs.io · github.com/Genesis-Embodied-AI/Genesis · github.com/taichi-dev/taichi · 80.lv (JangaFX 2026 roadmap) · quadspinner.com/Compare

**AI:** github.com/cumulo-autumn/StreamDiffusion · pypi.org/project/streamdiffusion · github.com/chenfengxu714/StreamDiffusionV2 · arxiv.org/abs/2511.07399 · github.com/daydreamlive/StreamDiffusion · github.com/daydreamlive/scope (+ LICENSE.md, releases) · huggingface.co/daydreamlive/scope · businesswire.com (Daydream Scope launch, Nov 2025) · dotsimulate.com/docs/streamdiffusiontd · blog.livepeer.org/builder-story-dotsimulate-x-daydream · github.com/olegchomp/TouchDiffusion · forum.vvvv.org (StreamDiffusion in vvvv) · docs.comfy.org (changelog, comms_routes, workflow_json, partner-nodes) · comfy.org/cloud/pricing · github.com/Acly/krita-ai-diffusion · github.com/livepeer/comfystream · krea.ai/blog/krea-realtime-14b · self-forcing.github.io · github.com/NVlabs/LongLive · github.com/NVlabs/Sana · arxiv.org/abs/2605.30409 · huggingface.co/stabilityai/{sd-turbo,sdxl-turbo,stable-diffusion-xl-base-1.0,stable-diffusion-3.5-large,stable-audio-open-1.0} · huggingface.co/black-forest-labs/{FLUX.1-schnell,FLUX.1-Kontext-dev} · huggingface.co/Wan-AI/Wan2.2-TI2V-5B · huggingface.co/microsoft/TRELLIS.2-4B · github.com/ByteDance-Seed/Depth-Anything-3 · github.com/facebookresearch/sam3 · github.com/magenta/magenta-realtime · github.com/openai/whisper · github.com/ggml-org/llama.cpp · github.com/ollama/ollama · github.com/leejet/stable-diffusion.cpp · learn.microsoft.com/windows/ai/new-windows-ml/supported-execution-providers · developer.nvidia.com/tensorrt-rtx · rocm.docs.amd.com (Windows compatibility) · fal.ai/docs/model-apis/real-time · replicate.com/pricing · runpod.io/pricing · docs.platform.decart.ai/getting-started/pricing · docs.rs/wgpu-cuda-interop · docs.rs/dlpack-sys

**Platform:** github.com/gfx-rs/wgpu (releases, CHANGELOG) · crates.io (egui, egui_dock, egui-snarl, slint, iced, xilem, vello, pyo3, rerun, bevy_hanabi, grafton-ndi, rusty_link, midir, midi2, sacn, artnet_protocol, ltc, isf, oscquery) · github.com/shader-slang/slang · bevy.org/news/bevy-0-20 · PyO3 CHANGELOG · peps.python.org/pep-0779 · dev-discuss.pytorch.org (3.13t) · pyappdist.readthedocs.io (macOS) · docs.ndi.video (licensing, distribution) · github.com/Ableton/link · spout.discourse.group (2.007.017) · github.com/leadedge/Spout2 · libfunnel.readthedocs.io · docs.rs/vk-video · developer.playcanvas.com (splat formats) · github.com/ArthurBrussee/brush · livedesignonline.com (LDI 2026 AI; Notch Builder)
