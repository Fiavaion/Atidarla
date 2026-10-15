# Atidarla

A free, open-source real-time visual instrument for **Windows and macOS** (Linux best-effort). Patch it like a node engine, play it like a clip deck, sequence it on a timeline, all over one graph, with GPU procedural geometry and AI you can perform.

> **Status: M0 (foundations & spikes).** The workspace, CI, app shell, capability flags, CLI bench and the AI Host protocol prototype exist. No operators yet; the first IMG and SIG operators arrive in M1. Progress: [`docs/milestones/M0.md`](docs/milestones/M0.md).

- Requirements and decisions: [`PRD.md`](PRD.md) (binding)
- Evidence behind each decision: [`RESEARCH.md`](RESEARCH.md)
- Working rules for contributors and agents: [`CLAUDE.md`](CLAUDE.md)

## Quick start

Prerequisites: Rust (the pinned toolchain in `rust-toolchain.toml` installs automatically through rustup), Python 3.11+ (3.13 recommended), and [uv](https://docs.astral.sh/uv/) for the AI Host.

```sh
cargo run -p atd-app                     # the application shell
cargo run -p atd-cli -- bench            # performance suite → JSON (FR-H4)
cargo run -p atd-cli -- registry         # operator schemas as JSON
cargo test --workspace                   # all Rust tests (needs Python on PATH for atd-py / atd-ai)
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check                         # licence policy (cargo install cargo-deny)

cd ai-host && uv sync && uv run pytest   # AI Host tests
cd ai-host && uv run atd-ai-host --dev   # run a host standalone
```

Set `ATD_PYTHON` to choose the interpreter the AI Host tests spawn (default: `python` on Windows, `python3` elsewhere).

**Linux (best-effort):** the app loads `libxkbcommon-x11` (X11) or the Wayland client libraries at runtime and needs a Vulkan driver. On Debian/Ubuntu: `apt install libxkbcommon-x11-0 mesa-vulkan-drivers`.

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
