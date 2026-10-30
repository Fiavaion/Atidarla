# Offline render (`atidarla render`, FR-H1)

`atidarla render` cooks a project offline at a fixed time step and writes the frames as a
numbered PNG sequence. It is the first slice of FR-H1 (PRD §7.10) and of M2 AC4: two renders of
the same project on the same machine give byte-identical frames.

## Usage

```sh
atidarla render examples/audio-reactive-feedback.atidarla --out renders/arf --range 0-300 --fixed-step
```

| Option | Meaning |
|---|---|
| `<project>` | Project folder (`*.atidarla`). |
| `--out <dir>` | Folder for the frames, created if missing: `frame_000000.png`, `frame_000001.png`, ... numbered by frame (a render of `10-20` writes `frame_000010.png` to `frame_000019.png`). Existing frames are overwritten. |
| `--range A-B` | Frames `A` (inclusive) to `B` (exclusive). Default `0-60`. |
| `--fps N` | Frame `i` is at `i / N` seconds. Default 60 (project files have no frame rate yet). |
| `--fixed-step` | Accepted for the PRD's command line; the step is always fixed. |
| `--node <name>` | The IMG node to render. Default: the node the app's viewer shows first (the last IMG node nothing reads from, else the last IMG node). An unknown name or a non-IMG node is an error that lists the IMG nodes. |

The resolution is the project's. Frames are 8-bit RGBA, as the engine renders them.

**Not in this slice:** writing a movie file. `--out clip.mov` (or `.mp4`, `.png` and other file
extensions) is refused with a message saying so; encode the PNG sequence with another tool for
now. Movie output needs two decisions of its own: HAP encoding (A15 "HAP-first") and FFmpeg
encoding (A23 covers decoding only).

Warnings (stderr) name every live input and every sink in the project (see below), and any node
this platform does not run (capability flags, A19), which renders as empty. Any other node error
stops the render with the frame number and the message.

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
- **Files** are read as they are when the render reaches them. A WGSL file is compiled once at the
  start; edits during a render are not picked up.

## Design note: how offline mode works

**The mechanism.** The engine has a cook mode, `CookMode::Live` (the default: the app, `check`,
`bench`) or `CookMode::Offline { fps }` (`render` only), set with `Engine::set_mode`. Operators
read it from their `Params` (`params.mode()`), which already carries the other engine-wide
settings an operator cooks under (project folder, expressions), so every context (`CookCtx`,
`SigCtx`, `ImgToSigCtx`) and `GpuOp::output_size` see it without new fields. Offline:

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
   own slot for this frame's result (bounded by their timeout, `docs/python-ops.md`).
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
| Pure | A function of parameters, inputs and the frame's time. | IMG: Blend, Bloom, Blur, Channel Image, Channel Mix, Chroma Key, Composite, Constant, Crop, Displace, Edge, HSV Adjust, Kaleidoscope, Level, Lookup, Luma Key, Mirror, Mixer, Noise, Normal Map, Projection, Ramp, Remap, Render Points, Resolution, Shape, Slope, Switch, Threshold, Tile, Transform. SIG: Constant, LFO, Limit, Math, Merge, Noise, Pattern, Select. PNT: Age, Attract, Attribute, Attribute Math, Bend, Collide, Colour by Attribute, Connect, Copy to Points, Delete, Drag, For-Each, For-Each End, Gravity, Group, Integrate, Merge, Noise, Normals, Piece, Points, Randomize, Scatter, Smooth, Sort, Subset, Taper, Transform, Turbulence, Twist, Vortex, Wave, Wind. Render (`img.render`). MAT: Line, Matcap, PBR, Point Sprite, Toon, Unlit. MSH: Platonic Solid. |
| Stateful | State from frame to frame, starting with the render: identical across runs of the same range; a range started part-way differs from a longer render. | IMG: Cache, Feedback, Fluid, Layer (the clip playing, a transition, a launch waiting for its beat or bar), Neural CA (also waits for its pipelines and weights file), Reaction-Diffusion, Ripple, Time Machine. SIG: Beat, Count, Envelope, Filter, Lag, Onset, Spectrum, Speed, Timer, Trail, Trigger. PNT: Emitter, Particles, Solver (and any GEO loop through a feedback input). |
| Waits | Work on a worker thread; offline the cook waits for it. | Kernel (`pnt.kernel`: compile; a file once; state on the wire), Movie In (opening and every frame; playhead `cue + time * speed`), Audio File In (decode; playhead from time), Text (rasteriser), WGSL (compile; a file once), Compute (compile; a file once; persistent `state` images and buffers also make it stateful), ISF shaders (compile; persistent buffers also make one stateful), Python operators (`py.*`, [`docs/python-ops.md`](python-ops.md): each frame waits for its own result, at most the operator's timeout plus 3 s; those with `state` are also stateful; they run only with `--allow-python-ops`). MSH: every operator (`msh.*`, [`design/msh-operators.md`](design/msh-operators.md)): cooked on a worker thread; offline the cook reads GPU inputs back and waits for the result, so frame *i* uses the inputs of frame *i*. |
| One frame late | Values are those of the previous frame, always exactly one. | Analyze (GPU readback). Also every Python expression. |
| Live input | Opens nothing; idle value; `render` warns. | Share In (black), Audio In (no channels), Input Devices (keys up, no gamepad, pointer at rest), MIDI In (no channels), OSC In (no channels), Timecode In (all zero, unlocked). |
| Sink | Passes its input through; emits nothing; `render` warns. | Share Out, Audio Out, DMX Out, MIDI Out, OSC Out. |

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
