# CLAUDE.md — Lasair

Lasair is an open-source (MIT OR Apache-2.0) real-time visual instrument for Windows and macOS (Linux best-effort): a node engine (Patch), a clip-based performance layer (Stage) and a timeline over one graph, with GPU procedural geometry and native AI. It supersedes NascD.

## Read first
- `PRD.md` — requirements, decision log (§0), milestones and acceptance criteria. **The PRD is binding.** If a task conflicts with it, stop and ask.
- `RESEARCH.md` — evidence behind each decision. Cite its section when you deviate or propose a change.
- Work one milestone at a time (M0 → M6). A milestone is done only when every acceptance criterion in PRD §9 passes and is recorded in `docs/milestones/Mn.md`.

## Stack
Rust (edition 2024) workspace · wgpu 30.x (pinned) · egui 0.37 + our own node canvas · PyO3 0.30, Python 3.13 default (floor 3.11) · WGSL + WESL · FFmpeg (LGPL build) · Python AI Host package managed with uv.

## Layout
`crates/lsr-*` (see PRD §5.2) · `ai-host/` (Python) · `packs/` (shipped Packs) · `examples/` · `docs/` (spikes, milestones, generated operator docs) · `bench/`.

## Commands
- Build/test: `cargo build --workspace` · `cargo test --workspace` · `cargo clippy --workspace -- -D warnings` · `cargo fmt --all` · `cargo deny check`
- Run: `cargo run -p lsr-app` · headless: `cargo run -p lsr-cli -- render|run|serve|bench ...`
- AI Host: `cd ai-host && uv sync && uv run lsr-ai-host --dev`

## Non-negotiable rules
1. **Never block the render thread** on Python, AI Hosts, MSH (CPU mesh) cooks, disk or network. AI is asynchronous and goes through the Output Governor; MSH cooks run on worker threads.
2. **Frame-critical paths allocate nothing after warm-up.** Use the pools in `lsr-gpu`.
3. **No GPL or non-commercial code in core crates.** ComfyUI stays out-of-process; Ableton Link and NDI live in `lsr-plugin-*`. `cargo deny` must pass.
4. **Never commit or bundle model weights.** Models go through the model manager with licence metadata.
5. **Clean-room AI.** Implement StreamDiffusion-style techniques from papers and Apache-2.0 sources only; never copy from closed or Patreon-gated tools.
6. **Windows + macOS parity.** Features land on both together, unless they are platform-exclusive capabilities (PRD A19, e.g. `win+nvidia` real-time diffusion, Kinect): declare the capability flag in the operator schema and implement the fallback (route to LAN/cloud, substitute, or hide with an explanation). Linux must not break the build where cheap to keep, but never blocks a milestone.
7. **Every operator ships complete:** schema, docs page, runnable example, unit test, cook-time budget.
8. **Project files stay text** with stable IDs and deterministic ordering; add round-trip tests for any format change.
9. **Agent tools are typed.** Do not add arbitrary code-execution MCP tools outside the opt-in `python.exec` scope.
10. No telemetry. No product UI uses competitors' trademarks or operator family names.

## Working style
- Before coding a milestone, write a short plan in `docs/milestones/Mn.md` mapping each acceptance criterion to tasks and tests.
- Prefer small, reviewable commits. Run fmt, clippy, tests and the relevant `lasair bench` cases before committing.
- When a decision in PRD §0 is still "Proposed", ask Mark before relying on it beyond M0.
- Record any new architectural decision in the PRD decision log with date and rationale.
