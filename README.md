# Atidarla

A free, open-source real-time visual instrument for **Windows and macOS** (Linux best-effort). Patch it like a node engine, play it like a clip deck, sequence it on a timeline, all over one graph, with GPU procedural geometry and AI you can perform.

> **Status: M0 (foundations & spikes), with M1 pre-work.** In place:
>
> - the workspace, with CI on Windows (WARP) and macOS (Metal);
> - capability flags, GPU texture sharing and the AI Host protocol prototype;
> - the graph engine (undo, pull cook, zero allocations per frame);
> - 32 IMG and 26 SIG operators;
> - ISF import (99% of the public collection), HAP Movie In, and live Audio In;
> - the Patch editor.
>
> Progress: [`docs/milestones/M0.md`](docs/milestones/M0.md), [`docs/milestones/M1.md`](docs/milestones/M1.md).

**Using the app:** `cargo run -p atd-app` opens a demo patch (noise, kaleidoscope and level, animated by two LFOs). Operator reference: [`docs/operators`](docs/operators/README.md).

| Do | How |
|---|---|
| Add an operator | Tab or double-click the canvas, type, Enter |
| Wire | drag from an output (right) to an input (left); right-click an input to disconnect |
| Select | click; Shift/Ctrl-click to add; Shift-drag a box; Ctrl+A |
| Edit | select a node: Viewer shows it, Parameters edits it (text fields commit on Enter) |
| Bind a parameter to a channel | the *bind…* menu, or drop a wire dragged from a SIG output (or a channel from the channel viewer) onto the parameter's name |
| Copy / paste / duplicate | Ctrl+C / Ctrl+V (system clipboard, works across windows) / Ctrl+D |
| Comment box | C wraps the selection; drag its title bar to move it with its nodes; double-click to edit |
| Delete, undo, redo | Delete; Ctrl+Z; Ctrl+Shift+Z |
| Navigate | drag the background or middle-drag to pan, scroll to zoom, click the minimap |
| Inspect | viewer modes RGBA / RGB / R / G / B / A and exposure (EV); hover for pixel coordinates |

Media: **Movie In** plays HAP / HAP Alpha / HAP Q / HAP R `.mov` files (decoded on the GPU as BC textures) and PNG/JPEG stills. **Audio In** captures the default input device (or `test-tone`). **Audio File In** plays WAV. **MIDI In/Out** and **OSC In/Out** connect controllers and other software; **DMX Out** drives lighting over Art-Net or sACN.

- Requirements and decisions: [`PRD.md`](PRD.md) (binding)
- Evidence behind each decision: [`RESEARCH.md`](RESEARCH.md)
- Working rules for contributors and agents: [`CLAUDE.md`](CLAUDE.md)

## Quick start

Prerequisites: Rust (the pinned toolchain in `rust-toolchain.toml` installs automatically through rustup), Python 3.11+ (3.13 recommended), and [uv](https://docs.astral.sh/uv/) for the AI Host.

```sh
cargo run -p atd-app                     # the application shell
cargo run -p atd-cli -- bench            # performance suite → JSON (FR-H4)
cargo run -p atd-cli -- registry         # operator schemas as JSON
cargo run -p atd-cli -- isf-import <dir> --report --gpu   # ISF shaders → WGSL, per-shader report
cargo test --workspace                   # all Rust tests (needs Python on PATH for atd-py / atd-ai)
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check                         # licence policy (cargo install cargo-deny)

cd ai-host && uv sync && uv run pytest   # AI Host tests
cd ai-host && uv run atd-ai-host --dev   # run a host standalone
```

To use ISF shaders in the app, set `ATIDARLA_ISF_PATH` to one or more directories of `.fs` files. Use `;` between directories on Windows and `:` elsewhere. Each shader that translates appears as an `img.isf_*` operator.

Set `ATD_PYTHON` to choose the interpreter the AI Host tests spawn (default: `python` on Windows, `python3` elsewhere).

**Linux (best-effort):** the app loads `libxkbcommon-x11` (X11) or the Wayland client libraries at runtime and needs a Vulkan driver. Building needs the ALSA headers for audio input. On Debian/Ubuntu: `apt install libxkbcommon-x11-0 mesa-vulkan-drivers libasound2-dev`.

## Layout

| Path | What |
|---|---|
| `crates/atd-core` | graph store, parameters, cook scheduler, **operator registry + capability flags** |
| `crates/atd-gpu`, `atd-kernel` | wgpu device, pools, render graph; Kernel DSL → WGSL |
| `crates/atd-ops-{img,sig,pnt,msh,mat,dat,cmp,ai}` | the eight operator families |
| `crates/atd-ai` | AI Host protocol, supervisor, router, Output Governor |
| `crates/atd-app`, `atd-cli` | application shell; `atidarla` headless CLI |
| `crates/atd-{io,interop,py,agent,stage,timeline,format}` | I/O, GPU sharing, Python, MCP agent, Stage, Timeline, project format |
| `crates/atd-plugin-{ndi,link}` | optional, separately licensed plugins |
| `ai-host/` | the Python AI Host (uv-managed) |
| `docs/` | milestones, spikes, generated operator docs |
| `packs/`, `examples/`, `bench/` | shipped Packs, example projects, bench results |

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. GPL and non-commercial components are kept out of process or in optional plugins (PRD §10). No model weights are ever bundled.
