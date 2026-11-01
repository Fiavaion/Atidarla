# Handoff: continuing M1 locally

> **2026-10-31 (newest; read this first):** M5 finished and M6 built on the M5 Pro Mac, overnight.
> - **M5:** AC1 eval met: 19/20, 18/20, 19/20 over three runs, strict criteria, qwen2.5-coder 7B (plan-then-build with Ollama structured output); cloud-model row needs an `ANTHROPIC_API_KEY`. In-app Assistant panel, in-show cues (A62), `create_pack`, Speech to Text and LLM Chat. See `docs/milestones/M5.md`.
> - **M6:** registry at 211 operators (MSH modelling and construction, VAT In, PNT capture/attributes/neighbours, Sensor In, point caches, Splat In + Splat MAT with GPU sort, DAT routing and 13 DAT ops, To Channels / Pixel Out / Resample); `lasair soak` (60-min PT-10 pass here); accessibility pass; installer scripts (DMG built unsigned; MSI never built); user manual. See `docs/milestones/M6.md`.
> - Later the same night: vector and colour parameters can be animated (A66); this lifted AC1 to 95/90/95% over three runs. Python-tier expressions and Script code now run only from a content allow-list (A67), which closes the "opening an untrusted project runs its Python" gap. It passed three security review rounds.
> - Proposed decisions for Mark: A62 (cues), A63 (point caches, Sensor In), A64 (DAT values, Script gating), A65 (GEO→SIG readback), A66 (animated vectors), A67 (network Python trust), and the eval-criteria changes in the M5 log.
> - Gate at `11fdb55`: `cargo test --workspace --no-fail-fast` 1085 passed, 0 failed; clippy, fmt, `docs --check`, `cargo deny` clean; `check --gpu` 69/69 examples.
> - This Mac after a restart: Homebrew's rustup puts no `cargo` on PATH; use `export PATH="$HOME/.rustup/toolchains/1.97.0-aarch64-apple-darwin/bin:$PATH"` and `PYO3_PYTHON=$(brew --prefix python@3.13)/bin/python3.13`, or the binary links the system Python 3.9 and fails to start.

> **2026-10-30:** a day on an Apple M5 Pro Mac (macOS 26.6, Metal). Mark ratified A9, A12, A24, A25 and the M2 proposals (A26–A52); A53–A56 were decided while building and are marked for review in `PRD.md` §0.
> - **M1:** Syphon Share In/Out works on macOS (official framework, A55); VideoToolbox decode checked on real Apple hardware. Open: the hand checks in `docs/milestones/M1.md` (AC1 in the app on Windows and Mac, PT-1 on the RTX 4070, Spout/Syphon with real apps).
> - **M2:** the Kernel DSL runs in the engine (`pnt.kernel`, project `kernels/*.atk` operators with `ch()` controls, `img.kernel`); AC2 (ten-line kernel = node chain) and AC3 (hot reload keeps state) have tests. Solver sub-steps, For-Each blocks and deformer fusion (5.7–6.7× on 30 transforms) are built. All 43+ examples render byte-identically on Metal; Boolean → UV → glTF opens in Blender. Open: PT-2/PT-12 on the RTX 4070. See `docs/milestones/M2.md` (pre-work log).
> - **M3:** every FR-S and FR-T requirement has a first implementation; see the "At a glance" table in `docs/milestones/M3.md`. PT-8 met here. Open: AC1 (scripted 60-minute set on the RTX 4070, runner not built), AC2 (show export Windows → Mac by hand), PT-7 numbers, the macOS multi-window repaint stall.
> - Gate at `6405b5a`: `cargo test --workspace` 723 passed, 0 failed (twice), clippy, fmt, `docs --check`, `check --gpu examples/*.lasair`, `cargo deny check`, app smoke runs. Two fresh-context reviews ran (kernel/live values; merged M3); their findings are fixed with regression tests.
> - Local set-up on a Mac: `brew install rustup python@3.13 pkgconf ffmpeg`; export `PYO3_PYTHON=$(brew --prefix python@3.13)/bin/python3.13`; Xcode with its Metal toolchain (Syphon builds from source).

> **2026-10-19:** the M2 foundations (geometry, PNT/MSH/MAT operators, 3D render, Packs, Python operators, NCA and compute, a Kernel DSL compiler, offline render) were built overnight and are on this branch, CI green. Start with [`docs/milestones/m2/REVIEW.md`](milestones/m2/REVIEW.md): what exists, what to try, the decisions needed and the caveats. M1 is still open on hardware checks (see `docs/milestones/M1.md`, "Hand checks for Mark"). The rest of this file describes the state on 2026-10-16.

Written 2026-10-16 when work moved from a cloud session to local VS Code + Claude Code.
Branch: `claude/compassionate-franklin-c9opw6` (no pull request opened yet).

## Where things stand

Read `docs/milestones/M1.md` first: the "At a glance" table, the decisions of 2026-10-16
(A11, A21–A23 in `PRD.md` §0), and the dated log at the bottom (newest first) with the
evidence for each change.

Done on this branch:

- **Project folders (A11, FR-F1)**: `crates/lsr-format` (TOML, canonical form, round-trip and
  diff tests); File → New/Open/Save/Save As in the app (`crates/lsr-app/src/project.rs`);
  `lasair check [--gpu]` and `lasair fmt` (`crates/lsr-cli/src/project.rs`).
- **Ten example projects** in `examples/` covering every IMG and SIG operator (a test enforces
  it). CI checks them on all OSes and cooks them on Windows and macOS.
- **AC1** test now loads the real example project; **AC2** met (327 shaders, A21);
  **AC4** met (one-line diffs).
- **Expressions (A22)**: native subset in `crates/lsr-core/src/expr.rs`, engine side in
  `crates/lsr-gpu/src/expr.rs`, Python worker in `crates/lsr-py/src/expr.rs`, `=` button in
  `crates/lsr-app/src/params.rs`. Reference: `docs/expressions.md`.
- **Offline render (M2 pre-work, FR-H1 first slice)**: `lasair render` writes a deterministic
  PNG sequence (`crates/lsr-cli/src/render.rs`). Reference: `docs/render.md`; evidence in the
  pre-work log of `docs/milestones/M2.md`.

## FFmpeg video (A23): done except packaging

- `crates/lsr-io/src/video/ffmpeg_shim.c` (C shim over FFmpeg: open, frame times, frame by
  index with keyframe seeking, RGBA via swscale, hardware decode with software fallback),
  `crates/lsr-io/build.rs` (pkg-config, or `FFMPEG_DIR` with `include/` and `lib/`),
  `crates/lsr-io/src/video/ffmpeg.rs` (Rust FFI) and `run_other` in `video/mod.rs`. Feature
  chain: `lsr-app`/`lsr-cli` `ffmpeg` → `lsr-ops-img/ffmpeg` → `lsr-io/ffmpeg`. Off by default.
- CI job `FFmpeg` builds, lints and tests it on Windows (BtbN LGPL shared 9.0), macOS
  (Homebrew) and Linux (apt). Hardware decode was checked on 2026-10-16: `d3d11va` on the
  RTX 5070 Ti PC, `videotoolbox` on the macOS runner, and on 2026-10-30 `videotoolbox` for
  H.264, VP9 and ProRes on a real Mac (Apple M5 Pro; see the M1 log).
- **Left: packaging (M6)**: ship FFmpeg's LGPL DLLs/dylibs beside the app, replaceable (A23).

To run it locally on Windows: download BtbN's `ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip`
(into the ignored `.scratch/`), then set `FFMPEG_DIR` to the unpacked folder, add its `bin\` to
`PATH`, and run `cargo test -p lsr-io --features ffmpeg --test ffmpeg -- --nocapture`.

## Other open M1 items

- **AC1 by hand**: build the audio-reactive feedback patch in the app on a Windows and a Mac
  machine, save, reload, compare (record in M1.md).
- **AC3 / PT-1**: `cargo run -p lsr-cli --release -- bench` on the RTX 4070 PC.
- **Share In/Out**: Spout is done on Windows (operators, example, verified against the official SDK; Windows runs on DX12, A24). Still open: Syphon on a Mac, and trying real apps (Resolume, TouchDesigner, OBS). Known: four or more WARP devices in parallel crash in WARP while D3D11 devices are created, so the Spout GPU tests run one at a time.
- Check CI for the newest push; the test fixes of 2026-10-16 are described in `M1.md`.
- **Mostly fixed 2026-10-17/18**: parallel GPU tests used to hang on NVIDIA/Windows when Vulkan and
  GL were both enabled; tests now use `lsr_gpu::primary_instance()` (DX12 on Windows, A24), and
  `.cargo/config.toml` caps test threads at 4 because 16 threads still stalled about 1 run in 20.
  Cause unknown. Unfixed:
  `midi::opening_without_devices_...` missed its deadline twice after long idle periods (not
  reproducible; see the M1 log). Re-run it before suspecting a real regression.

## Local set-up

- Rust 1.97 (`rust-toolchain` is pinned in CI), Python **3.13** on `PATH` (the app, CLI and
  `lsr-py` embed it through PyO3), and for the AI Host `uv`.
- Linux only: `libasound2-dev libudev-dev`.
- FFmpeg development files only for `--features lsr-io/ffmpeg`.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
cargo run -p lsr-cli -- docs --check
cargo run -p lsr-cli -- check --gpu examples/*.lasair
cargo run -p lsr-app -- examples/audio-reactive-feedback.lasair
cargo test -p lsr-io --features ffmpeg --test ffmpeg -- --nocapture
```

Follow `CLAUDE.md` (binding rules) and keep logging changes with evidence in `M1.md`.
