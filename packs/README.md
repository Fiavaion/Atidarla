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
| [Feedback Trails](feedback_trails/help.md) | 1.0.0 | Fading, zooming, hue-shifting trails | [pack-feedback-trails](../examples/pack-feedback-trails.atidarla/) |
| [Kaleidoscope Tunnel](kaleido_tunnel/help.md) | 1.0.0 | A self-running kaleidoscope generator | [pack-kaleido-tunnel](../examples/pack-kaleido-tunnel.atidarla/) |
| [Neon Edges](neon_edges/help.md) | 1.0.0 | Glowing, saturated outlines over the image | [pack-neon-edges](../examples/pack-neon-edges.atidarla/) |
| [Vignette](vignette/help.md) | 1.0.0 | Darkens or tints the edges of an image | [pack-vignette](../examples/pack-vignette.atidarla/) |

Checks (CI runs them on Windows and macOS):

```sh
cargo run -p atd-cli -- check packs/*/          # valid and canonical
cargo run -p atd-cli -- check --gpu packs/*/    # also cook one instance of each
cargo run -p atd-cli -- fmt packs/glow          # canonical form after hand edits
```

A test (`shipped_packs_are_complete_and_their_examples_current` in `crates/atd-cli`) requires
every Pack here to be locked, to have a summary, a label, promoted parameters and a help page
starting with a title, and an example project whose copy equals this one.
