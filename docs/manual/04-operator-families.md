# 4. Operator families

Every node is an instance of an **operator**. Operators come in eight **families**. A family
says what kind of data its nodes mostly handle. The family is the first part of an operator's
type name: `img.noise`, `sig.lfo`, `pnt.age`.

The full list, with every parameter, input and output, is generated from the program and kept
in the [operator reference](../operators/README.md). This chapter explains what the families
are for and how they plug together.

## Wire types

Each socket has a **wire type**. A wire joins two sockets of the same type only.

| Wire type | Carries |
|---|---|
| **Img** | A picture, on the GPU |
| **Sig** | Named channels of numbers, one value per frame (audio nodes carry audio-rate samples) |
| **Geo** | Points and meshes (PNT and MSH share this one type) |
| **Dat** | Text and tables |
| **Mat** | A material: how a surface looks |
| **Cmp** | A wire type for the CMP family (the built-in Window node works on Img wires) |

The wire type decides what you can connect. If you try to join mismatched sockets, the edit is
refused with a message. A wire that would make a loop is also refused, except into a *feedback*
input, which reads the previous frame instead (see [Feedback](../operators/img/feedback.md)).

One thing crosses between types without a wire:

- A **binding** lets a SIG channel drive a parameter ([chapter 5](05-expressions-bindings.md)).

## IMG: pictures

49 operators. Images are made and changed on the GPU. This is where most beginners start.

- **Generators** make pictures from nothing: [Noise](../operators/img/noise.md),
  [Ramp](../operators/img/ramp.md), [Shape](../operators/img/shape.md),
  [Constant](../operators/img/constant.md), [Text](../operators/img/text.md).
- **Movies and stills**: [Movie In](../operators/img/movie_in.md) (HAP natively; more with FFmpeg).
- **Effects** change a picture: [Blur](../operators/img/blur.md),
  [Bloom](../operators/img/bloom.md), [Level](../operators/img/level.md),
  [HSV Adjust](../operators/img/hsv_adjust.md), [Edge](../operators/img/edge.md),
  [Kaleidoscope](../operators/img/kaleidoscope.md), [Transform](../operators/img/transform.md).
- **Combining**: [Composite](../operators/img/composite.md), [Blend](../operators/img/blend.md),
  [Switch](../operators/img/switch.md), keys ([Chroma Key](../operators/img/chroma_key.md),
  [Luma Key](../operators/img/luma_key.md)).
- **Memory**: [Feedback](../operators/img/feedback.md), [Cache](../operators/img/cache.md),
  [Time Machine](../operators/img/time_machine.md), and simulations such as
  [Reaction-Diffusion](../operators/img/reaction_diffusion.md) and
  [Fluid 2D](../operators/img/fluid.md).
- **Your own shaders**: [WGSL](../operators/img/wgsl.md), [Compute](../operators/img/compute.md),
  [Kernel](../operators/img/kernel.md) (chapter 6).
- **Stage**: [Mixer](../operators/img/mixer.md) and [Layer](../operators/img/layer.md) (chapter 3).
- **Rendering 3D**: [Render](../operators/img/render.md) and
  [Render Points](../operators/img/render_points.md) turn geometry into a picture.
- **Sharing and lights**: [Share In](../operators/img/share_in.md),
  [Share Out](../operators/img/share_out.md) and [Pixel Map](../operators/img/pixel_map.md)
  (chapter 7).

## SIG: signals

29 operators. Signals are numbers that change over time, grouped into named **channels**. They
are how Atidarla listens to the world and how it animates.

- **Oscillators and clocks**: [LFO](../operators/sig/lfo.md), [Noise](../operators/sig/noise.md),
  [Beat](../operators/sig/beat.md), [Timer](../operators/sig/timer.md),
  [Pattern](../operators/sig/pattern.md).
- **Shaping**: [Math](../operators/sig/math.md), [Filter](../operators/sig/filter.md),
  [Lag](../operators/sig/lag.md), [Limit](../operators/sig/limit.md),
  [Trail](../operators/sig/trail.md), [Merge](../operators/sig/merge.md),
  [Select](../operators/sig/select.md).
- **Triggers**: [Trigger](../operators/sig/trigger.md), [Count](../operators/sig/count.md),
  [Onset](../operators/sig/onset.md).
- **Audio**: [Audio In](../operators/sig/audio_in.md), [Audio File In](../operators/sig/audio_file_in.md),
  [Audio Out](../operators/sig/audio_out.md), [Spectrum](../operators/sig/spectrum.md),
  [Envelope](../operators/sig/envelope.md).
- **Control and lights**: [MIDI In](../operators/sig/midi_in.md), [OSC In](../operators/sig/osc_in.md),
  [Input Devices](../operators/sig/input.md), [Timecode In](../operators/sig/timecode_in.md),
  [DMX Out](../operators/sig/dmx_out.md).

SIG outputs are what you pick in a parameter's **bind...** menu.

## PNT: points

54 operators. Points are lists of positions with attributes (colour, velocity, age, ...),
processed on the GPU, so they can be very many. Use them for particles and point clouds.

Typical chain: [Emitter](../operators/pnt/emitter.md) or [Scatter](../operators/pnt/scatter.md)
makes points; forces such as [Gravity](../operators/pnt/gravity.md) and
[Turbulence](../operators/pnt/turbulence.md) move them; [Age](../operators/pnt/age.md) removes
old ones; [Render Points](../operators/img/render_points.md) draws them. There is also a
[Solver](../operators/pnt/solver.md) for simulation loops, [Particles](../operators/pnt/particles.md),
[Point File In](../operators/pnt/file_in.md) for point caches ([`point-caches.md`](../point-caches.md)),
and [Sensor In](../operators/pnt/sensor_in.md) for depth and body tracking. Live depth capture
needs a Windows-only SDK. On other systems Sensor In plays recordings and a built-in test scene.

Example: [`pnt-fountain`](../../examples/pnt-fountain.atidarla/).

## MSH: meshes

41 operators. Meshes are surfaces made of points and faces, built on the CPU (on worker
threads, so they never freeze the picture). Use them for 3D shapes.

Make a shape ([Box](../operators/msh/box.md), [Sphere](../operators/msh/sphere.md),
[Torus](../operators/msh/torus.md), [Grid](../operators/msh/grid.md)), change it
([Extrude](../operators/msh/extrude.md), [Subdivide](../operators/msh/subdivide.md),
[Boolean](../operators/msh/boolean.md), [Transform](../operators/msh/transform.md)), or read
and write files ([Mesh File In](../operators/msh/file_in.md),
[Mesh File Out](../operators/msh/file_out.md)). Then wire it into
[Render](../operators/img/render.md).

PNT and MSH share the **Geo** wire, so their nodes connect to each other. The engine looks
after whether the data sits on the CPU or the GPU (PRD decision A10). Mesh baking is an
exception: it needs the mesh on the CPU, and a node from the point family has it only on the
GPU ([chapter 13](13-command-line.md)).

Examples: [`mesh-primitives`](../../examples/mesh-primitives.atidarla/),
[`render-solids`](../../examples/render-solids.atidarla/).

## MAT: materials

7 operators: [PBR](../operators/mat/pbr.md), [Matcap](../operators/mat/matcap.md),
[Toon](../operators/mat/toon.md), [Unlit](../operators/mat/unlit.md),
[Line](../operators/mat/line.md), [Point Sprite](../operators/mat/point_sprite.md) and
[Splat](../operators/mat/splat.md). A material output (**Mat**) goes into the `mat` input
that sits beside each `geo` input of [Render](../operators/img/render.md). Render with no
material uses a plain grey surface.

## DAT: text and tables

13 operators. DAT wires carry text and tables. Use them for prompts, notes, data files and
network messages.

- **Typed in**: [Text](../operators/dat/text.md), [Table](../operators/dat/table.md).
- **Reshape**: [Select](../operators/dat/select.md), [Convert](../operators/dat/convert.md),
  [JSON](../operators/dat/json.md), [Prompt Template](../operators/dat/prompt_template.md).
- **Files**: [File In](../operators/dat/file_in.md), [File Out](../operators/dat/file_out.md).
- **Outside world**: [Web Client](../operators/dat/web_client.md),
  [Web Server](../operators/dat/web_server.md), [WebSocket](../operators/dat/websocket.md),
  [Serial](../operators/dat/serial.md).
- **Code**: [Script](../operators/dat/script.md) (Python, needs your permission; chapter 6).

Slow work (files, the web, a serial port) runs on worker threads and never holds up the picture.
Examples: [`dat-tables`](../../examples/dat-tables.atidarla/),
[`dat-files`](../../examples/dat-files.atidarla/). The AI nodes
[Speech to Text](../operators/ai/stt.md) and [LLM Chat](../operators/ai/llm_chat.md) also use
DAT wires (chapter 8).

## CMP: compound and output nodes

1 built-in operator today: [Window](../operators/cmp/window.md), an output to a projector or
screen (chapter 7). **Packs** also live in this family: a Pack instance in a network is a node
of type `cmp.pack_<name>` (chapter 6).

## AI: models

14 operators run AI models. They work **asynchronously** on a separate program, the AI Host, so
a slow model never freezes the picture. See [chapter 8](08-ai.md).

[Diffusion](../operators/ai/diffusion.md), [Video Model](../operators/ai/video_model.md),
[AI Model](../operators/ai/model.md), [Prompt Clip](../operators/ai/prompt.md); vision nodes
[Depth](../operators/ai/depth.md), [Segment](../operators/ai/segment.md),
[Background Remove](../operators/ai/background.md), [Pose](../operators/ai/pose.md),
[Optical Flow](../operators/ai/optical_flow.md), [Upscale](../operators/ai/upscale.md),
[ONNX Inference](../operators/ai/onnx.md), [Control Prep](../operators/ai/control_prep.md);
and [Speech to Text](../operators/ai/stt.md) and [LLM Chat](../operators/ai/llm_chat.md).

## Nodes that may not run on your computer

Some operators need a capability that only some computers have, for example real-time local
diffusion needs a Windows PC with an NVIDIA GPU. Each operator page has a **Platforms**
section that says so. On other computers the node uses a stated fallback: it routes to another
host, stands in with a simpler version, or is hidden with an explanation.
`atidarla check --gpu` lists such nodes as "not available on this platform" instead of failing.

## Where project and custom operators appear

Operators you write yourself appear in the Operators panel under **Project**: kernels
(`kernel.<name>`), Python operators (`py.<name>`) and ComfyUI workflows (`comfy.<name>`).
Chapters 6 and 8 cover them.
