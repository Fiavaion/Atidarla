# Packs

Shipped Packs: reusable nested networks with promoted parameters, a semantic version and
embedded help (PRD §5.5, FR-P4). One folder per Pack:

```text
glow/
  pack.toml      name, version, label, summary, lock, promoted parameters, sockets
  network.toml   the network inside (the same form as a project's networks/*.toml)
  help.md        the help page shown in the app
```

An older version kept beside the current one goes in `<name>@<version>/`. A project that uses
a Pack carries its own copy in `<project>/packs/<name>/`; updating it to a newer library version
is an explicit step that reports what changed. The model and the file format are described in
[`docs/packs.md`](../docs/packs.md).

| Pack | Version | What it does | Example |
|---|---|---|---|
| [Audio Pulse](audio_pulse/help.md) | 1.0.0 | Brightens and glows an image with the loudness of an audio signal; outputs the pulse | [pack-audio-pulse](../examples/pack-audio-pulse.atidarla/) |
| [Beat Flash](beat_flash/help.md) | 1.0.0 | A tempo clock flashing an image on every beat; outputs the flash | [pack-beat-flash](../examples/pack-beat-flash.atidarla/) |
| [Chromatic Aberration](chromatic_aberration/help.md) | 1.0.0 | Splits red and blue apart | [pack-chromatic-aberration](../examples/pack-chromatic-aberration.atidarla/) |
| [Colour Grade](colour_grade/help.md) | 1.0.0 | Exposure, contrast, gamma, saturation, hue and a tint in one node | [pack-colour-grade](../examples/pack-colour-grade.atidarla/) |
| [Curl Swarm](curl_swarm/help.md) | 1.0.0 | A self-running particle swarm in swirling curl noise | [pack-curl-swarm](../examples/pack-curl-swarm.atidarla/) |
| [Duotone](duotone/help.md) | 1.0.0 | Recolours an image in two colours | [pack-duotone](../examples/pack-duotone.atidarla/) |
| [Feedback Trails](feedback_trails/help.md) | 1.0.0 | Fading, zooming, hue-shifting trails | [pack-feedback-trails](../examples/pack-feedback-trails.atidarla/) |
| [Film Grain](film_grain/help.md) | 1.0.0 | Per-frame grain, flicker and faded colour | [pack-film-grain](../examples/pack-film-grain.atidarla/) |
| [Kaleidoscope Tunnel](kaleido_tunnel/help.md) | 1.0.1 | A self-running kaleidoscope generator | [pack-kaleido-tunnel](../examples/pack-kaleido-tunnel.atidarla/) |
| [LFO Bank](lfo_bank/help.md) | 1.0.0 | Four oscillators spread in phase; a SIG modulation source | [pack-lfo-bank](../examples/pack-lfo-bank.atidarla/) |
| [Neon Edges](neon_edges/help.md) | 1.1.0 | Glowing, saturated outlines over the image | [pack-neon-edges](../examples/pack-neon-edges.atidarla/) |
| [RGB Delay](rgb_delay/help.md) | 1.0.0 | Green and blue delayed behind red: colour ghosts on movement | [pack-rgb-delay](../examples/pack-rgb-delay.atidarla/) |
| [Spark Fountain](spark_fountain/help.md) | 1.0.0 | A self-running particle fountain, coloured by speed | [pack-spark-fountain](../examples/pack-spark-fountain.atidarla/) |
| [Spinning Solid](spinning_solid/help.md) | 1.0.0 | A lit 3D solid turning, with an optional texture input | [pack-spinning-solid](../examples/pack-spinning-solid.atidarla/) |
| [Vignette](vignette/help.md) | 1.1.0 | Darkens or tints the edges of an image | [pack-vignette](../examples/pack-vignette.atidarla/) |

Checks (CI runs them on Windows and macOS):

```sh
cargo run -p atd-cli -- check packs/*/          # valid and canonical
cargo run -p atd-cli -- check --gpu packs/*/    # also cook one instance of each
cargo run -p atd-cli -- fmt packs/glow          # canonical form after hand edits
```

A test (`shipped_packs_are_complete_and_their_examples_current` in `crates/atd-cli`) requires
every Pack here to be locked, to have a summary, a label, promoted parameters and a help page
starting with a title, and an example project whose copy equals this one.
