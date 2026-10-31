# Offline render (`atidarla render`, FR-H1)

`atidarla render` cooks a project offline at a fixed time step and writes the frames as a
numbered PNG sequence, or, in a build with FFmpeg, as a movie file (FR-S10, below). It is the first slice of FR-H1 (PRD §7.10) and of M2 AC4: two renders of
the same project on the same machine give byte-identical frames.

## Usage

```sh
atidarla render examples/audio-reactive-feedback.atidarla --out renders/arf --range 0-300 --fixed-step
```

| Option | Meaning |
|---|---|
| `<project>` | Project folder (`*.atidarla`). |
| `--out <dir>` | Folder for the frames, created if missing: `frame_000000.png`, `frame_000001.png`, ... numbered by frame (a render of `10-20` writes `frame_000010.png` to `frame_000019.png`). Existing frames are overwritten. |
| `--out <file.mov\|file.mp4>` | A movie file instead (FFmpeg builds only; see [Movie files](#movie-files-fr-s10)). |
| `--codec <name>` | Movie codec: `prores` (ProRes 422, default for `.mov`), `prores-hq`, `prores4444` (with alpha), `h264` (default for `.mp4`), `h265`. |
| `--bitrate <bit/s>` | H.264/H.265 bit rate; default about 0.15 bit per pixel per frame (25 Mbit/s at 1080p60). |
| `--range A-B` | Frames `A` (inclusive) to `B` (exclusive). Default `0-60`. |
| `--fps N` | Frame `i` is at `i / N` seconds. Default 60 (project files have no frame rate yet). |
| `--fixed-step` | Accepted for the PRD's command line; the step is always fixed. |
| `--node <name>` | The IMG node to render. Default: the node the app's viewer shows first (the last IMG node nothing reads from, else the last IMG node). An unknown name or a non-IMG node is an error that lists the IMG nodes. |

The resolution is the project's. Frames are 8-bit RGBA, as the engine renders them.

Other file extensions (`.png`, `.gif`, `.webm`, ...) are refused with a message: pass a folder
for frames.

## Movie files (FR-S10)

With FFmpeg (`cargo build -p atd-cli --features atd-io/ffmpeg`; PRD A23, A51), `--out` may name
a `.mov` or `.mp4` file:

```sh
atidarla render examples/stage-deck.atidarla --out renders/deck.mov --range 0-600
atidarla render examples/stage-deck.atidarla --out renders/deck.mp4 --codec h265 --bitrate 40000000
```

| Codec | FFmpeg encoder | Container | Notes |
|---|---|---|---|
| `prores` | `prores_ks`, profile Standard | `.mov` | 10-bit 4:2:2. Every FFmpeg build has it. |
| `prores-hq` | `prores_ks`, profile HQ | `.mov` | |
| `prores4444` | `prores_ks`, profile 4444, 16-bit alpha | `.mov` | Keeps the image's alpha. |
| `h264` | `h264_videotoolbox` (macOS); `h264_nvenc`, then `h264_mf` (Windows) | `.mp4`, `.mov` | Hardware. One keyframe a second, no B-frames. |
| `h265` | `hevc_videotoolbox` (macOS); `hevc_nvenc`, then `hevc_mf` (Windows) | `.mp4`, `.mov` | Hardware. Tagged `hvc1` for Apple players. |

**Only LGPL encoders.** A23/A51 and CLAUDE.md rule 3: Atidarla never asks FFmpeg for
`libx264` or `libx265` (GPL), so an LGPL FFmpeg build (no `--enable-gpl`) is enough, and a GPL
build's extra encoders are never used. H.264 and H.265 therefore need the platform's hardware
encoder: VideoToolbox on every Mac; NVENC on NVIDIA, Media Foundation on other Windows GPUs.
The Windows encoder names are compiled in and tried in order; a machine without them gets a
message naming them. Linux tries NVENC only (best effort, A19).

**No HAP encoder.** HAP (A15) would need FFmpeg's `hap` encoder, which needs Snappy
(`--enable-libsnappy`, BSD) at FFmpeg build time; Homebrew's FFmpeg and the usual LGPL builds
are configured without it (checked 2026-10-30: `ffmpeg -encoders` lists no `hap`). A native
HAP encoder (DXT1/DXT5 + Snappy, both already used for decoding in `atd-io`) is the way to add
it without depending on how FFmpeg was built; PRD A51 leaves it for later.

**Colour.** Frames are 8-bit RGBA, full range, treated as BT.709 (sRGB primaries). They are
converted to YUV with the BT.709 matrix at limited range, and the stream is tagged BT.709, so
players and Atidarla's Movie In convert back with the same matrix. Alpha is kept only by
`prores4444`; the other codecs drop it (the colour is written as is, not over black).

**Determinism.** The PNG path is unchanged and byte-identical across runs (below). Movies are
encoded from the same frames in order (the renderer waits for the encoder, so no frame is
dropped), but encoders, hardware ones especially, do not promise identical bytes; compare
movies by decoding them.

**Timing.** The frame rate is `--fps` (as a fraction: 29.97 is 29970/1000). Frame *i* has
presentation time *i / fps*, starting at 0 whatever `--range` starts at.

**Without FFmpeg** (the default build), `--out clip.mov` is refused with a message saying how
to rebuild, or to pass a folder.

### Recording live (the app)

The Viewer's toolbar has **Record**: choose a `.mov` (ProRes 422) or `.mp4` (H.264) file, and
every app frame of the viewed image (view the Mixer to record the composition) is written at 60
frames a second until **Stop**. The image is copied to the CPU through a ring of three readback
buffers mapped asynchronously and encoded on a worker thread, so recording never makes the
render thread wait (rule 1); when the GPU copies or the encoder fall behind, frames are dropped
and counted in the status (`REC 12.0 s, 720 frames, 3 dropped`). The recording assumes the app
runs at 60 fps: at a lower rate the movie plays faster than real time. Changing the viewed
image's size stops the recording. Code: `crates/atd-app/src/record.rs` (`RecordPanel`: `start`,
`frame`, `stop`, usable by other panels such as the Stage); the encoder is
`atd_io::video::record::Recorder`.

Warnings (stderr) name every live input and every sink in the project (see below), and any node
this platform does not run (capability flags, A19), which renders as empty. Any other node error
stops the render with the frame number and the message.

## Baking a network (`atidarla bake`)

`bake` runs the same offline engine (same fixed step, same determinism) but writes one node's
output by its type, so geometry can leave the engine as files:

```sh
atidarla bake examples/mesh-primitives.atidarla --node scene --out baked --range 0-120 --format glb
atidarla bake examples/mesh-primitives.atidarla --node ball --out baked    # static: baked/mesh.ply
```

| Option | Meaning |
|---|---|
| `--node <name>` | Any GEO (MSH) or IMG node. Default: the last GEO node nothing reads from, else the last IMG node. |
| `--out <dir>` | Folder, created if missing (default `bake`). |
| `--range A-B` | Frames `A` to `B`, end exclusive (default `0-1`, one frame). |
| `--fps N` | As in `render`. |
| `--format` | `obj`, `ply` (default, keeps every attribute), `gltf`, `glb`: the Mesh File Out writers. |
| `--sequence` | Write `mesh_000000.ply`, ... even when every frame is identical. |

| | `render` | `bake` |
|---|---|---|
| Output | IMG node only: PNG frames or a movie | IMG node: the same PNG frames. GEO/MSH node: mesh files |
| Meshes | not available | per frame as `mesh_000012.ply` (frame number); if every frame in the range is byte-identical (a static network) one `mesh.ply` |
| Movies | `.mov` / `.mp4` | no |

Mesh baking needs the node's attributes on the CPU, which MSH operators always have; a GEO
node from the point (PNT) family has them only on the GPU and fails with an error that says so.
Files written by `msh.file_out` nodes inside the project still write to their own paths while
baking, as in a render. Files read back with Mesh File In / `atd-ops-msh`'s reader give the baked mesh.

## What is deterministic

Two renders of the same project and range on **the same machine, GPU adapter, graphics backend,
driver and build** give byte-identical frames. That is the promise; nothing more:

- **Not across machines, adapters or backends.** GPUs and drivers round differently (for example
  0.5 in 8 bits is 127 on NVIDIA and 128 on WARP and Metal), and shader compilers optimise
  differently. Windows renders on DX12 by default (A24); `WGPU_BACKEND` changes that, and the
  frames with it. The render prints the adapter and backend it used.
- **Ranges and state.** Every frame is cooked at its own time `i / fps`, so a stateless project
  gives the same frame `i` whatever range it is part of and whatever was rendered before (tested:
  frames 10-19 rendered alone, backwards, on an engine warm from another project, equal those of
  a `0-20` render). Stateful operators (Feedback, Cache, Time Machine, the simulations, SIG
  integrators such as Timer, Speed, Beat, Lag; the "stateful" rows below) start their state with
  the render, so frames 10-19 of `--range 10-20` legitimately differ from the same frames of
  `--range 0-20`. Each is still identical across runs of the same range.
- **Python expressions** are one frame late, always exactly one (frame 0 shows the stored value).
  Python code that reads the clock, `random` without a seed, files or the network is not
  deterministic; the native expression subset is.
- **The timeline** (`timeline.toml`, [`timeline.md`](timeline.md)) is evaluated at each frame's
  time: tracks become live values, and every cue action (`set`, `live`, `launch`) is held as a
  live value rather than edited into the network. A frame rendered alone first fires the timed
  cues before it, so it matches the same frame of a whole render (tested). Cues with a condition
  read the previous frame's channels (the nodes they name cook with the output) and fire as
  crossed; a range started part-way arms them rather than firing them.
- **The Stage** plays what the layers' `target` parameters (and the timeline's launch cues) say.
  The autopilot, one-shot returns and flash releases are app behaviour and do not run in
  `atidarla render`.
- **Files** are read as they are when the render reaches them. A WGSL file is compiled once at the
  start; edits during a render are not picked up.

## Design note: how offline mode works

**The mechanism.** The engine has a cook mode, `CookMode::Live` (the default: the app, `check`,
`bench`) or `CookMode::Offline { fps }` (`render` only), set with `Engine::set_mode`. Operators
read it from their `Params` (`params.mode()`), which already carries the other engine-wide
settings an operator cooks under (project folder, expressions), so every context (`CookCtx`,
`SigCtx`, `GpuToSigCtx`) and `GpuOp::output_size` see it without new fields. Offline:

1. **Fixed time.** The renderer calls `Engine::set_frame(i)` and cooks at `i / fps`; the engine
   gives every SIG operator `dt = 1 / fps`, the first frame included. Operators that measured
   their own step (ISF `TIMEDELTA`, Fluid) use the step too; ISF `FRAMEINDEX` is the frame number
   and `DATE` is 1970-01-01 00:00 UTC plus the frame's time, not the clock. Movie In's and Audio
   File In's playheads are functions of time (`cue + time * speed`; the samples between one step
   ago and now) rather than sums of steps.
2. **Wait for workers.** Work that live runs on worker threads and uses "whenever it arrives" is
   waited for. The uniform hook is `CookMode::receive(&rx)`: `try_recv` live, `recv` offline; the
   operators that hand results over a channel (Audio File In's decoder, the ISF, WGSL and Analyze
   pipeline builders, Text's rasteriser) call it instead of `try_recv`. The two that do not use a
   channel wait their own way: Movie In waits for the file header and each frame
   (`MoviePlayer::wait_ready` / `wait_frame`, at most 60 s, then a node error), and Python
   expressions go through `PythonHost::submit_wait`, which returns once the worker has evaluated
   the frame's batch, so the next frame always has every result. That wait is bounded too
   (`atd_py::expr::OFFLINE_TIMEOUT`, 10 s): expressions still running after it fail on their
   nodes, so the render stops with a message instead of hanging. Python operators wait on their
   own slot for this frame's result (bounded by their timeout, `docs/python-ops.md`). Hosted AI
   nodes wait for their host's answer to the previous frame (`Link::wait_result`, at most 120 s)
   only while a host on the route is connected or still starting, so a slow host still answers
   frame `i - 1` on frame `i`. When no host on the route can run the node (none configured, it
   failed to start or refused the connection, it lacks the node's adapter) or the host is lost
   before it answers, the node errors at once with what to do. A host that refuses to load the
   model is tried again (it may still be downloading it) until the 30 s give-up.
3. **Fixed latency for readbacks.** Analyze reads back on the GPU asynchronously. Offline it waits
   (`device.poll(wait)`) for the previous frame's readback before reading it, so its channels are
   always the measurement of the frame before: exactly one frame late, as Python expressions.
4. **Inputs idle, sinks silent.** Live inputs (audio, MIDI, OSC, input devices, timecode, Share
   In) open nothing and output their idle value (no channels, zeros, black); sinks (OSC, DMX, MIDI
   and audio out, Share Out) pass their input through and emit nothing. `render` warns about each.
   `render` also cooks only what the rendered node needs; sinks elsewhere do not cook at all.
5. **No pool history.** Pooled textures hold whatever they last held. Every operator writes its
   whole output, and offline the engine also clears each output before the operator cooks, so no
   frame can depend on what the pool held before.
6. **Deterministic order** needs nothing new: the scheduler plans in a fixed order from the
   network (nodes by ID, inputs by name, expression dependencies sorted).

**Why this design.** The alternatives were (a) a generic "settle" loop that re-cooks a frame until
no operator is pending, as `atidarla check --gpu` does in real time; and (b) defining every async
result as "the one requested a frame earlier", delivered deterministically. (a) breaks stateful
operators (Feedback and every SIG integrator would advance on each re-cook) and Analyze is never
idle on a changing image. (b) makes every movie, compile and decode a frame late offline, which
is not what an offline render is for. Waiting at the point where each operator takes its result
keeps one code path per operator for both modes, changes nothing live (the live branch is the
code that was there), and leaves a one-frame latency only where the data path itself is a frame
behind (GPU readback, Python). Live, nothing waits and nothing allocates on the cook path (rules 1
and 2); offline, nothing is real time, so rule 1 is relaxed inside this mode only.

**Adding an operator.** Classify it in `classify()` in `crates/atd-cli/src/render.rs` (a test
fails until every registered operator is) and in the table below. If it uses a worker thread,
take results with `params.mode().receive(&rx)`; if it reads a device, the network or another
application, output the idle value when `params.mode().is_offline()`; if it is a sink, pass
through and do not emit offline.

## Operators

| Class | Offline behaviour | Operators |
|---|---|---|
| Pure | A function of parameters, inputs and the frame's time. | IMG: Blend, Bloom, Blur, Channel Image, Channel Mix, Chroma Key, Composite, Constant, Crop, Displace, Edge, HSV Adjust, Kaleidoscope, Level, Lookup, Luma Key, Mirror, Mixer, Noise, Normal Map, Projection, Ramp, Remap, Render Points, Resolution, Shape, Slope, Switch, Threshold, Tile, Transform. SIG: Constant, LFO, Limit, Math, Merge, Noise, Pattern, Select. PNT: Age, Attract, Attribute, Attribute from Channels, Attribute from Image, Attribute Math, Attribute Noise, Attribute Promote, Attribute Transfer, Bend, Collide, Colour by Attribute, Connect, Copy to Points, Delete, Drag, Falloff Field, Flock, For-Each, For-Each End, Gravity, Group, Integrate, Lattice, Merge, Neighbours, Noise, Normals, Piece, Points, Points from Depth, Points from Image, Proximity, Randomize, Ray Intersect, Scatter, Smooth, Sort, Subset, Taper, Transform, Turbulence, Twist, Vortex, Wave, Wind. Render (`img.render`). MAT: Line, Matcap, PBR, Point Sprite, Splat, Toon, Unlit. MSH: Platonic Solid. DAT: Table, Text, JSON, Convert, Select, Prompt Template. CMP: Window (renders the mapped output image; the native window is the app's, `docs/outputs.md`). |
| Stateful | State from frame to frame, starting with the render: identical across runs of the same range; a range started part-way differs from a longer render. | IMG: Cache, Feedback, Fluid, Layer (the clip playing, a transition, a launch waiting for its beat or bar), Neural CA (also waits for its pipelines and weights file), Reaction-Diffusion, Ripple, Time Machine. SIG: Beat, Count, Envelope, Filter, Lag, Onset, Spectrum, Speed, Timer, Trail, Trigger. PNT: Emitter, Particles, Solver (and any GEO loop through a feedback input), Trail (its history starts with the render). |
| Waits | Work on a worker thread; offline the cook waits for it. | Kernel (`pnt.kernel` and `img.kernel`: compile; a file once; a Points kernel's state on the wire), project kernel operators (`kernel.*`, [`docs/kernel-dsl.md`](kernel-dsl.md): the same, every `kernels/` file read once when the render starts), Movie In (opening and every frame; playhead `cue + time * speed`), Audio File In (decode; playhead from time), Text (rasteriser), WGSL (compile; a file once), Compute (compile; a file once; persistent `state` images and buffers also make it stateful), ISF shaders (compile; persistent buffers also make one stateful), Python operators (`py.*`, [`docs/python-ops.md`](python-ops.md): each frame waits for its own result, at most the operator's timeout plus 3 s; those with `state` are also stateful; they run only with `--allow-python-ops`), ComfyUI workflow operators (`comfy.*`, [`docs/comfy.md`](comfy.md): a frame whose parameters or input images changed reads its inputs back and waits for its run on the node's server; other frames reuse the last result; no server is an error). Point File In (`pnt.file_in`: every file of a sequence read on a worker thread; offline frame *i* waits for its file), Sensor In (`pnt.sensor_in`: its recordings, as Point File In; the test scene is a function of time and frame; live capture outputs nothing offline). DAT: File In (reads its file on the cook before the first frame), Script (each run waits for its result, at most 30 s; it runs only with `--allow-python-ops`). MSH: every operator (`msh.*`, [`design/msh-operators.md`](design/msh-operators.md)): cooked on a worker thread; offline the cook reads GPU inputs back and waits for the result, so frame *i* uses the inputs of frame *i*. Resample (`pnt.resample`: cooked on a worker thread like the MSH operators, the same way). Splat In (`pnt.splat_in`: the file read and decoded once). To Channels (`pnt.to_channels`: a GPU readback of points, PRD A65; offline the cook submits the frame so far and waits for its copy, so frame *i* outputs frame *i*'s points, never a frame late). |
| One frame late | Values are those of the previous frame, always exactly one. | Analyze (GPU readback). Also every Python expression. |
| Live input | Opens nothing; idle value; `render` warns. | Share In (black), Audio In (no channels), Input Devices (keys up, no gamepad, pointer at rest), MIDI In (no channels), OSC In (no channels), Timecode In (all zero, unlocked), Web Client (no request; empty). |
| Sink | Passes its input through; emits nothing; `render` warns. | Share Out, Audio Out, DMX Out, MIDI Out, OSC Out, File Out (writes nothing), Web Server (does not listen), WebSocket (does not connect), Serial (does not open its port). Speech to Text and LLM Chat send nothing and publish nothing (their channels stay at rest). Pixel Map sends nothing; its channels (cooked only when something rendered reads them) are its readback, exactly one frame late. Pixel Out (`pnt.pixel_out`) sends nothing; its channels are frame *i*'s points, waited for as To Channels does. |

Beat stays Stateful with every source: offline its `midi` source reads no port (the clock sits
at beat 0, and `render` does not warn: Beat is not a live input in the table), its MIDI clock out
sends nothing, and its `audio` source follows its SIG input, which is as deterministic as that
input. A synced LFO or Timer is a function of the clock it reads, so two offline renders give
identical channels (`cargo test -p atd-ops-sig --test clock_offline`).

## Evidence

Tests (`cargo test -p atd-cli render`, `cargo test -p atd-py --test engine`; GPU tests skip
without an adapter unless `ATD_REQUIRE_GPU` is set): two renders of the AC1 example are identical
and audio-driven; the same render with every core busy is identical and has the decoded audio from
frame 0; frames do not depend on render history (above); live inputs are idle, open nothing (the
OSC port stays free) and are named; Share Out publishes no Spout sender offline (Windows); Python
results are exactly one frame late with a slow expression; every registered operator is
classified. CI renders the AC1 example twice for 30 frames on Windows and macOS and compares the
PNG bytes. Hash comparisons of real CLI runs are in the M2 pre-work log
(`docs/milestones/M2.md`).
