# Examples

Runnable example projects (`*.atidarla/` folders, PRD §5.5). Open one in the app with
**File → Open Project…**, or from a terminal:

```sh
cargo run -p atd-app -- examples/audio-reactive-feedback.atidarla
```

Each project's `project.toml` says what it shows and what to try. Between them they use
every IMG and SIG operator (CLAUDE.md rule 7); each operator's page in
[`docs/operators`](../docs/operators/README.md) links the examples that use it.

| Project | Shows |
|---|---|
| [audio-reactive-feedback](audio-reactive-feedback.atidarla/) | The M1 AC1 patch: a feedback loop following a drum loop's spectrum |
| [beat-switcher](beat-switcher.atidarla/) | A BPM clock switching generators per beat, with triggers and counters |
| [stage-deck](stage-deck.atidarla/) | A Stage deck: three layers by four columns, quantised launch, fades and wipes, launch modes and a crossfader (M3); play it in the Stage tab |
| [ai-live](ai-live.atidarla/) | Every AI operator (M4): Diffusion with Prompt Blend and a Control Prep edge map, the built-in Invert model, the ONNX vision nodes and Video Model, governed at the project frame rate |
| [speech-chat](speech-chat.atidarla/) | Speech to Text (Whisper) feeding LLM Chat (Ollama) through `Prompt From`, both on this machine, so Sovereign mode stays on (FR-A13) |
| [batch-wedge](batch-wedge.atidarla/) | Batch Graph (M5): a noise seed × scale wedge through the built-in Invert model, and a 10 prompts × 10 seeds wedge across two LAN hosts; results land in the Stage tab's Media Bin ([`docs/batch.md`](../docs/batch.md)) |
| [beat-clock](beat-clock.atidarla/) | Tap tempo, nudge, resync and MIDI clock in/out on Beat, with beat-synced LFOs and a Timer (M3) |
| [two-outputs](two-outputs.atidarla/) | One composition to two output windows (`cmp.window`): slices, a corner pin, a Catmull-Rom warp, a mask and soft edges over the overlap (M3, [`docs/outputs.md`](../docs/outputs.md)) |
| [kaleidoscope-tunnel](kaleidoscope-tunnel.atidarla/) | Noise through kaleidoscope, mirror, tile, hue and bloom |
| [keying](keying.atidarla/) | Chroma and luma keys on a still |
| [image-processing](image-processing.atidarla/) | The filters: blur, edges, relief, normals, lookup, displace, remap |
| [simulations](simulations.atidarla/) | Reaction-diffusion, fluid and ripple solvers |
| [time-effects](time-effects.atidarla/) | Frame delay and slit-scan from a HAP movie |
| [signal-scope](signal-scope.atidarla/) | Audio and control signals drawn as images, and image analysis |
| [shaders](shaders.atidarla/) | A project-local ISF shader, inline WGSL and projection |
| [compute-shaders](compute-shaders.atidarla/) | WGSL compute shaders: a blur, an iterated diffusion, and trails with a state image and a buffer from a file |
| [neural-growth](neural-growth.atidarla/) | Neural cellular automata growing, regrowing where erased, and running a weights file |
| [live-control](live-control.atidarla/) | Mouse, OSC, MIDI, DMX, timecode and audio output |
| [pixel-map](pixel-map.atidarla/) | LED pixel mapping: a patch of strips, a pixel grid and PARs sampling an image into DMX universes ([`docs/dmx.md`](../docs/dmx.md)) |
| [texture-sharing](texture-sharing.atidarla/) | Share Out and Share In: publishing to other applications and a loopback (Spout on Windows) |
| [python-ops](python-ops.atidarla/) | Operators written in Python in the project's `ops/` folder: a counter, an LFO and an image generator ([`docs/python-ops.md`](../docs/python-ops.md)) |
| [points](points.atidarla/) | GPU geometry: point clouds deformed by curl noise and drawn as sprites (M2) |
| [particles](particles.atidarla/) | A GPU particle system whose state flows through a force on a feedback wire (M2) |
| [render-solids](render-solids.atidarla/) | 3D: lit Platonic solids under an orbiting camera, PBR and emission through Bloom (M2) |
| [render-textures](render-textures.atidarla/) | 3D: a textured square (uv) and a matcap-shaded solid (M2) |
| [render-wire-toon](render-wire-toon.atidarla/) | 3D: toon shading, a wireframe and points as lit sprites in one depth buffer (M2) |
| [splats](splats.atidarla/) | Gaussian splats from PLY, SPZ and SOG files (Splat In), GPU-sorted and blended by Splat MAT around a solid ring (M6) |
| [mesh-primitives](mesh-primitives.atidarla/) | MSH primitives merged, coloured, pulsing and exported to glTF, drawn as points (M2) |
| [mesh-modelling](mesh-modelling.atidarla/) | Extrude, Subdivide, Boolean, Delete and PolyReduce (M2) |
| [mesh-scatter-iso](mesh-scatter-iso.atidarla/) | Scatter → Iso Surface → curl-noise deform: the PNT↔MSH chain, and a Sweep (M2) |
| [mesh-files](mesh-files.atidarla/) | Mesh File In/Out (OBJ in, PLY out), Normals, Skin and UV Project (M2) |
| [mesh-boolean-gltf](mesh-boolean-gltf.atidarla/) | Boolean → Fuse → UV Project → Mesh File Out as a GLB that opens in Blender (M2) |
| [mesh-shaping](mesh-shaping.atidarla/) | Clip with a capped cut, Measure, Smooth, Remesh, Mirror, Clean and Bevel (M6) |
| [mesh-construction](mesh-construction.atidarla/) | Text 3D → UV Unwrap, Fracture, Carve → Revolve and Convex Hull (M6) |
| [mesh-vat](mesh-vat.atidarla/) | VAT In playing a soft-body ripple (16-bit PNG) and rigid cubes (EXR position and rotation) in the Houdini VAT 3.0 layout (M6, [`docs/design/vat-in.md`](../docs/design/vat-in.md)) |
| [timeline-show](timeline-show.atidarla/) | A 32-second show on the timeline: keyframe tracks (linear, Bezier, step), markers, timed, conditional and manual cues (M3) |
| [pnt-fountain](pnt-fountain.atidarla/) | Emitter, Gravity, Vortex, Turbulence, Wind, Drag and Collide in a particle loop (M2) |
| [pnt-deformers](pnt-deformers.atidarla/) | Twist, Taper, Bend and Wave on a column; Copy to Points with random sizes and colours (M2) |
| [pnt-constellation](pnt-constellation.atidarla/) | Smooth, Normals, Sort and Connect on the GPU grid hash; lines in Render Points (M2) |
| [pnt-solver](pnt-solver.atidarla/) | A Solver loop (Attract, Turbulence, Drag, Integrate, Age) over pieces coloured per piece (M2) |
| [pnt-foreach](pnt-foreach.atidarla/) | For-Each blocks: six counted copies of a column, and a strip coloured band by band (pieces) (M2) |
| [kernel-particles](kernel-particles.atidarla/) | A ten-line Kernel DSL particle simulation: `kernels/curl.atk` as the project's own operator, its `ch()` parameters as controls, plus an inline Kernel ([`docs/kernel-dsl.md`](../docs/kernel-dsl.md)) (M2) |
| [kernel-pixels](kernel-pixels.atidarla/) | Pixel kernels: an inline generator and a colour grade from `kernels/grade.pixels.atk` with its `ch()` controls (M2) |
| [comfy-invert](comfy-invert.atidarla/) | A ComfyUI workflow as an operator: `comfy/invert.json` (Load Image → Invert Image → Save Image) runs on your ComfyUI server; without one the node says so ([`docs/comfy.md`](../docs/comfy.md)) (M4) |

One example per shipped Pack ([`packs/`](../packs/README.md), `docs/packs.md`). Each project
carries its own copy of the Pack in `packs/<name>/` (a test keeps it equal to the library's):

| Project | Shows |
|---|---|
| [pack-audio-pulse](pack-audio-pulse.atidarla/) | Audio Pulse: a drum loop brightening noise; an expression reads the Pack's `level` channel |
| [pack-beat-flash](pack-beat-flash.atidarla/) | Beat Flash: flashes and kicks on every beat at 128 BPM |
| [pack-chromatic-aberration](pack-chromatic-aberration.atidarla/) | Chromatic Aberration: an LFO bound to a promoted parameter |
| [pack-colour-grade](pack-colour-grade.atidarla/) | Colour Grade: a warm and a cold grade of the same noise, side by side |
| [pack-curl-swarm](pack-curl-swarm.atidarla/) | Curl Swarm: a magenta and teal swarm whose pull breathes through an expression |
| [pack-duotone](pack-duotone.atidarla/) | Duotone: clouds and a title in magenta and cyan |
| [pack-feedback-trails](pack-feedback-trails.atidarla/) | Feedback Trails: an orbiting triangle leaving a hue-shifting spiral |
| [pack-film-grain](pack-film-grain.atidarla/) | Film Grain: a title card with grain, flicker and faded colour |
| [pack-kaleido-tunnel](pack-kaleido-tunnel.atidarla/) | Kaleidoscope Tunnel: two instances of one Pack with different settings |
| [pack-lfo-bank](pack-lfo-bank.atidarla/) | LFO Bank: four phase-spread channels turning, recolouring and fading a picture |
| [pack-neon-edges](pack-neon-edges.atidarla/) | Neon Edges: glowing outlines around a shape and text |
| [pack-rgb-delay](pack-rgb-delay.atidarla/) | RGB Delay: turning shapes fanning out into red, green and blue copies |
| [pack-spark-fountain](pack-spark-fountain.atidarla/) | Spark Fountain: sparks bouncing off the floor; an LFO bound to the spread |
| [pack-spinning-solid](pack-spinning-solid.atidarla/) | Spinning Solid: an icosahedron with noise wired into its texture input |
| [pack-vignette](pack-vignette.atidarla/) | Vignette: edges tinted towards deep blue |

Checks (CI runs both on Windows and macOS):

```sh
cargo run -p atd-cli -- check examples/*.atidarla        # load, validate, canonical form
cargo run -p atd-cli -- check --gpu examples/*.atidarla  # also cook every node
cargo run -p atd-cli -- check --gpu --allow-python-ops examples/*.atidarla  # and run ops/*.py
cargo run -p atd-cli -- fmt my-show.atidarla             # canonical form after hand edits
```

Python operators are code that runs with your privileges: `check` runs them only with
`--allow-python-ops` (CI passes it for these examples), and the app asks first.

Media in the examples is made for them (`beat.wav` and `greenscreen.png` are generated;
`testsrc.mov` is FFmpeg's built-in test pattern encoded as HAP; the Neural CA weights
`nca/coral.json` are hand-built by Atidarla's own generator, not trained), so it carries no
third-party content.
