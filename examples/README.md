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
| [kaleidoscope-tunnel](kaleidoscope-tunnel.atidarla/) | Noise through kaleidoscope, mirror, tile, hue and bloom |
| [keying](keying.atidarla/) | Chroma and luma keys on a still |
| [image-processing](image-processing.atidarla/) | The filters: blur, edges, relief, normals, lookup, displace, remap |
| [simulations](simulations.atidarla/) | Reaction-diffusion, fluid and ripple solvers |
| [time-effects](time-effects.atidarla/) | Frame delay and slit-scan from a HAP movie |
| [signal-scope](signal-scope.atidarla/) | Audio and control signals drawn as images, and image analysis |
| [shaders](shaders.atidarla/) | A project-local ISF shader, inline WGSL and projection |
| [live-control](live-control.atidarla/) | Mouse, OSC, MIDI, DMX, timecode and audio output |
| [texture-sharing](texture-sharing.atidarla/) | Share Out and Share In: publishing to other applications and a loopback (Spout on Windows) |

Checks (CI runs both on Windows and macOS):

```sh
cargo run -p atd-cli -- check examples/*.atidarla        # load, validate, canonical form
cargo run -p atd-cli -- check --gpu examples/*.atidarla  # also cook every node
cargo run -p atd-cli -- fmt my-show.atidarla             # canonical form after hand edits
```

Media in the examples is made for them (`beat.wav` and `greenscreen.png` are generated;
`testsrc.mov` is FFmpeg's built-in test pattern encoded as HAP), so it carries no
third-party content.
