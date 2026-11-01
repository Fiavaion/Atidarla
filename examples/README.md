# Examples

Runnable example projects (`*.lasair/` folders, PRD §5.5). Open one in the app with
**File → Open Project…**, or from a terminal:

```sh
cargo run -p lsr-app -- examples/audio-reactive-feedback.lasair
```

Each project's `project.toml` says what it shows and what to try. Between them they use
every IMG and SIG operator (CLAUDE.md rule 7); each operator's page in
[`docs/operators`](../docs/operators/README.md) links the examples that use it.

| Project | Shows |
|---|---|
| [audio-reactive-feedback](audio-reactive-feedback.lasair/) | The M1 AC1 patch: a feedback loop following a drum loop's spectrum |
| [beat-switcher](beat-switcher.lasair/) | A BPM clock switching generators per beat, with triggers and counters |
| [stage-deck](stage-deck.lasair/) | A Stage deck: three layers by four columns, quantised launch, fades and wipes, launch modes and a crossfader (M3); play it in the Stage tab |
| [soak-show](soak-show.lasair/) | The PT-10 soak patch: the Stage deck plus a 400,000-particle system and an image feedback loop as a fifth column (M6); `lasair soak` runs it for hours |
| [ai-live](ai-live.lasair/) | Every AI operator (M4): Diffusion with Prompt Blend and a Control Prep edge map, the built-in Invert model, the ONNX vision nodes and Video Model, governed at the project frame rate |
| [speech-chat](speech-chat.lasair/) | Speech to Text (Whisper) feeding LLM Chat (Ollama) over a DAT wire, both on this machine, so Sovereign mode stays on (FR-A13) |
| [dat-tables](dat-tables.lasair/) | Text and tables on DAT wires: Table, Text, Select, Convert, JSON and a Prompt Template filled from a table, text and a channel, and an expression reading a table cell |
| [dat-files](dat-files.lasair/) | File In reading a CSV and a JSON file (reloaded on save), Select and JSON over them, and File Out writing the result |
| [dat-network](dat-network.lasair/) | A Web Server on loopback serving a DAT, a Web Client polling it into JSON, and a WebSocket client (inactive until given a server) |
| [dat-serial](dat-serial.lasair/) | A serial device's lines as table rows, filtered with Select, and commands written back to it |
| [dat-script](dat-script.lasair/) | Script DATs: Python `cook(inputs)` over a table and a text, gated like the project's Python operators |
| [batch-wedge](batch-wedge.lasair/) | Batch Graph (M5): a noise seed × scale wedge through the built-in Invert model, and a 10 prompts × 10 seeds wedge across two LAN hosts; results land in the Stage tab's Media Bin ([`docs/batch.md`](../docs/batch.md)) |
| [beat-clock](beat-clock.lasair/) | Tap tempo, nudge, resync and MIDI clock in/out on Beat, with beat-synced LFOs and a Timer (M3) |
| [two-outputs](two-outputs.lasair/) | One composition to two output windows (`cmp.window`): slices, a corner pin, a Catmull-Rom warp, a mask and soft edges over the overlap (M3, [`docs/outputs.md`](../docs/outputs.md)) |
| [kaleidoscope-tunnel](kaleidoscope-tunnel.lasair/) | Noise through kaleidoscope, mirror, tile, hue and bloom |
| [keying](keying.lasair/) | Chroma and luma keys on a still |
| [image-processing](image-processing.lasair/) | The filters: blur, edges, relief, normals, lookup, displace, remap |
| [simulations](simulations.lasair/) | Reaction-diffusion, fluid and ripple solvers |
| [time-effects](time-effects.lasair/) | Frame delay and slit-scan from a HAP movie |
| [signal-scope](signal-scope.lasair/) | Audio and control signals drawn as images, and image analysis |
| [shaders](shaders.lasair/) | A project-local ISF shader, inline WGSL and projection |
| [compute-shaders](compute-shaders.lasair/) | WGSL compute shaders: a blur, an iterated diffusion, and trails with a state image and a buffer from a file |
| [neural-growth](neural-growth.lasair/) | Neural cellular automata growing, regrowing where erased, and running a weights file |
| [live-control](live-control.lasair/) | Mouse, OSC, MIDI, DMX, timecode and audio output |
| [pixel-map](pixel-map.lasair/) | LED pixel mapping: a patch of strips, a pixel grid and PARs sampling an image into DMX universes ([`docs/dmx.md`](../docs/dmx.md)) |
| [texture-sharing](texture-sharing.lasair/) | Share Out and Share In: publishing to other applications and a loopback (Spout on Windows) |
| [python-ops](python-ops.lasair/) | Operators written in Python in the project's `ops/` folder: a counter, an LFO and an image generator ([`docs/python-ops.md`](../docs/python-ops.md)) |
| [points](points.lasair/) | GPU geometry: point clouds deformed by curl noise and drawn as sprites (M2) |
| [particles](particles.lasair/) | A GPU particle system whose state flows through a force on a feedback wire (M2) |
| [render-solids](render-solids.lasair/) | 3D: lit Platonic solids under an orbiting camera, PBR and emission through Bloom (M2) |
| [render-textures](render-textures.lasair/) | 3D: a textured square (uv) and a matcap-shaded solid (M2) |
| [render-wire-toon](render-wire-toon.lasair/) | 3D: toon shading, a wireframe and points as lit sprites in one depth buffer (M2) |
| [splats](splats.lasair/) | Gaussian splats from PLY, SPZ and SOG files (Splat In), GPU-sorted and blended by Splat MAT around a solid ring (M6) |
| [mesh-primitives](mesh-primitives.lasair/) | MSH primitives merged, coloured, pulsing and exported to glTF, drawn as points (M2) |
| [mesh-modelling](mesh-modelling.lasair/) | Extrude, Subdivide, Boolean, Delete and PolyReduce (M2) |
| [mesh-scatter-iso](mesh-scatter-iso.lasair/) | Scatter → Iso Surface → curl-noise deform: the PNT↔MSH chain, and a Sweep (M2) |
| [mesh-files](mesh-files.lasair/) | Mesh File In/Out (OBJ in, PLY out), Normals, Skin and UV Project (M2) |
| [mesh-boolean-gltf](mesh-boolean-gltf.lasair/) | Boolean → Fuse → UV Project → Mesh File Out as a GLB that opens in Blender (M2) |
| [mesh-shaping](mesh-shaping.lasair/) | Clip with a capped cut, Measure, Smooth, Remesh, Mirror, Clean and Bevel (M6) |
| [mesh-construction](mesh-construction.lasair/) | Text 3D → UV Unwrap, Fracture, Carve → Revolve and Convex Hull (M6) |
| [mesh-vat](mesh-vat.lasair/) | VAT In playing a soft-body ripple (16-bit PNG) and rigid cubes (EXR position and rotation) in the Houdini VAT 3.0 layout (M6, [`docs/design/vat-in.md`](../docs/design/vat-in.md)) |
| [timeline-show](timeline-show.lasair/) | A 32-second show on the timeline: keyframe tracks (linear, Bezier, step), markers, timed, conditional and manual cues (M3) |
| [pnt-fountain](pnt-fountain.lasair/) | Emitter, Gravity, Vortex, Turbulence, Wind, Drag and Collide in a particle loop (M2) |
| [pnt-deformers](pnt-deformers.lasair/) | Twist, Taper, Bend and Wave on a column; Copy to Points with random sizes and colours (M2) |
| [pnt-constellation](pnt-constellation.lasair/) | Smooth, Normals, Sort and Connect on the GPU grid hash; lines in Render Points (M2) |
| [pnt-fields](pnt-fields.lasair/) | Falloff Field, Attribute Noise and Attribute from Channels on a grid; Attribute Transfer and Proximity from wandering brushes; Neighbours counting a swarm (M6) |
| [pnt-motion](pnt-motion.lasair/) | A Lattice deforming a ball by a drifting cage (and the cage drawn), boids (Flock) in a Solver loop kept centred by Attribute Promote and drawn as Trails, points shrink-wrapped onto a mesh by Ray Intersect (M6) |
| [pnt-capture](pnt-capture.lasair/) | Points from Image and from Depth, Attribute from Image, Point File In (CSV, PLY and a CSV sequence) and Sensor In's test scene with body tracking (M6, [`docs/point-caches.md`](../docs/point-caches.md)) |
| [pnt-outputs](pnt-outputs.lasair/) | Resample evens out a ring; To Channels reads point positions back as channels; Pixel Out lights a 60-pixel DMX strip with the points' colours (PRD A65, [`docs/dmx.md`](../docs/dmx.md)) |
| [pnt-solver](pnt-solver.lasair/) | A Solver loop (Attract, Turbulence, Drag, Integrate, Age) over pieces coloured per piece (M2) |
| [pnt-foreach](pnt-foreach.lasair/) | For-Each blocks: six counted copies of a column, and a strip coloured band by band (pieces) (M2) |
| [kernel-particles](kernel-particles.lasair/) | A ten-line Kernel DSL particle simulation: `kernels/curl.atk` as the project's own operator, its `ch()` parameters as controls, plus an inline Kernel ([`docs/kernel-dsl.md`](../docs/kernel-dsl.md)) (M2) |
| [kernel-pixels](kernel-pixels.lasair/) | Pixel kernels: an inline generator and a colour grade from `kernels/grade.pixels.atk` with its `ch()` controls (M2) |
| [comfy-invert](comfy-invert.lasair/) | A ComfyUI workflow as an operator: `comfy/invert.json` (Load Image → Invert Image → Save Image) runs on your ComfyUI server; without one the node says so ([`docs/comfy.md`](../docs/comfy.md)) (M4) |

One example per shipped Pack ([`packs/`](../packs/README.md), `docs/packs.md`). Each project
carries its own copy of the Pack in `packs/<name>/` (a test keeps it equal to the library's):

| Project | Shows |
|---|---|
| [pack-audio-pulse](pack-audio-pulse.lasair/) | Audio Pulse: a drum loop brightening noise; an expression reads the Pack's `level` channel |
| [pack-beat-flash](pack-beat-flash.lasair/) | Beat Flash: flashes and kicks on every beat at 128 BPM |
| [pack-chromatic-aberration](pack-chromatic-aberration.lasair/) | Chromatic Aberration: an LFO bound to a promoted parameter |
| [pack-colour-grade](pack-colour-grade.lasair/) | Colour Grade: a warm and a cold grade of the same noise, side by side |
| [pack-curl-swarm](pack-curl-swarm.lasair/) | Curl Swarm: a magenta and teal swarm whose pull breathes through an expression |
| [pack-duotone](pack-duotone.lasair/) | Duotone: clouds and a title in magenta and cyan |
| [pack-feedback-trails](pack-feedback-trails.lasair/) | Feedback Trails: an orbiting triangle leaving a hue-shifting spiral |
| [pack-film-grain](pack-film-grain.lasair/) | Film Grain: a title card with grain, flicker and faded colour |
| [pack-kaleido-tunnel](pack-kaleido-tunnel.lasair/) | Kaleidoscope Tunnel: two instances of one Pack with different settings |
| [pack-lfo-bank](pack-lfo-bank.lasair/) | LFO Bank: four phase-spread channels turning, recolouring and fading a picture |
| [pack-neon-edges](pack-neon-edges.lasair/) | Neon Edges: glowing outlines around a shape and text |
| [pack-rgb-delay](pack-rgb-delay.lasair/) | RGB Delay: turning shapes fanning out into red, green and blue copies |
| [pack-spark-fountain](pack-spark-fountain.lasair/) | Spark Fountain: sparks bouncing off the floor; an LFO bound to the spread |
| [pack-spinning-solid](pack-spinning-solid.lasair/) | Spinning Solid: an icosahedron with noise wired into its texture input |
| [pack-vignette](pack-vignette.lasair/) | Vignette: edges tinted towards deep blue |

Checks (CI runs both on Windows and macOS):

```sh
cargo run -p lsr-cli -- check examples/*.lasair        # load, validate, canonical form
cargo run -p lsr-cli -- check --gpu examples/*.lasair  # also cook every node
cargo run -p lsr-cli -- check --gpu --allow-python-ops examples/*.lasair  # and run ops/*.py
cargo run -p lsr-cli -- fmt my-show.lasair             # canonical form after hand edits
```

Python operators are code that runs with your privileges: `check` runs them only with
`--allow-python-ops` (CI passes it for these examples), and the app asks first.

Media in the examples is made for them (`beat.wav` and `greenscreen.png` are generated;
`testsrc.mov` is FFmpeg's built-in test pattern encoded as HAP; the Neural CA weights
`nca/coral.json` are hand-built by Lasair's own generator, not trained), so it carries no
third-party content.
