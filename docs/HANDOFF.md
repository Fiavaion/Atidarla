# Handoff: continuing M1 locally

> **2026-10-19:** the M2 foundations (geometry, PNT/MSH/MAT operators, 3D render, Packs, Python operators, NCA and compute, a Kernel DSL compiler, offline render) were built overnight and are on this branch, CI green. Start with [`docs/milestones/m2/REVIEW.md`](milestones/m2/REVIEW.md): what exists, what to try, the decisions needed and the caveats. M1 is still open on hardware checks (see `docs/milestones/M1.md`, "Hand checks for Mark"). The rest of this file describes the state on 2026-10-16.

Written 2026-10-16 when work moved from a cloud session to local VS Code + Claude Code.
Branch: `claude/compassionate-franklin-c9opw6` (no pull request opened yet).

## Where things stand

Read `docs/milestones/M1.md` first: the "At a glance" table, the decisions of 2026-10-16
(A11, A21–A23 in `PRD.md` §0), and the dated log at the bottom (newest first) with the
evidence for each change.

Done on this branch:

- **Project folders (A11, FR-F1)**: `crates/atd-format` (TOML, canonical form, round-trip and
  diff tests); File → New/Open/Save/Save As in the app (`crates/atd-app/src/project.rs`);
  `atidarla check [--gpu]` and `atidarla fmt` (`crates/atd-cli/src/project.rs`).
- **Ten example projects** in `examples/` covering every IMG and SIG operator (a test enforces
  it). CI checks them on all OSes and cooks them on Windows and macOS.
- **AC1** test now loads the real example project; **AC2** met (327 shaders, A21);
  **AC4** met (one-line diffs).
- **Expressions (A22)**: native subset in `crates/atd-core/src/expr.rs`, engine side in
  `crates/atd-gpu/src/expr.rs`, Python worker in `crates/atd-py/src/expr.rs`, `=` button in
  `crates/atd-app/src/params.rs`. Reference: `docs/expressions.md`.
- **Offline render (M2 pre-work, FR-H1 first slice)**: `atidarla render` writes a deterministic
  PNG sequence (`crates/atd-cli/src/render.rs`). Reference: `docs/render.md`; evidence in the
  pre-work log of `docs/milestones/M2.md`.

## FFmpeg video (A23): done except packaging

- `crates/atd-io/src/video/ffmpeg_shim.c` (C shim over FFmpeg: open, frame times, frame by
  index with keyframe seeking, RGBA via swscale, hardware decode with software fallback),
  `crates/atd-io/build.rs` (pkg-config, or `FFMPEG_DIR` with `include/` and `lib/`),
  `crates/atd-io/src/video/ffmpeg.rs` (Rust FFI) and `run_other` in `video/mod.rs`. Feature
  chain: `atd-app`/`atd-cli` `ffmpeg` → `atd-ops-img/ffmpeg` → `atd-io/ffmpeg`. Off by default.
- CI job `FFmpeg` builds, lints and tests it on Windows (BtbN LGPL shared 9.0), macOS
  (Homebrew) and Linux (apt). Hardware decode was checked on 2026-10-16: `d3d11va` on the
  RTX 5070 Ti PC, `videotoolbox` on the macOS runner (see the M1 log).
- **Left: packaging (M6)**: ship FFmpeg's LGPL DLLs/dylibs beside the app, replaceable (A23).
  **Not yet checked on a real Mac** (only the CI runner): `videotoolbox` on Apple hardware.

To run it locally on Windows: download BtbN's `ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip`
(into the ignored `.scratch/`), then set `FFMPEG_DIR` to the unpacked folder, add its `bin\` to
`PATH`, and run `cargo test -p atd-io --features ffmpeg --test ffmpeg -- --nocapture`.

## Other open M1 items

- **AC1 by hand**: build the audio-reactive feedback patch in the app on a Windows and a Mac
  machine, save, reload, compare (record in M1.md).
- **AC3 / PT-1**: `cargo run -p atd-cli --release -- bench` on the RTX 4070 PC.
- **Share In/Out**: Spout is done on Windows (operators, example, verified against the official SDK; Windows runs on DX12, A24). Still open: Syphon on a Mac, and trying real apps (Resolume, TouchDesigner, OBS). Known: four or more WARP devices in parallel crash in WARP while D3D11 devices are created, so the Spout GPU tests run one at a time.
- Check CI for the newest push; the test fixes of 2026-10-16 are described in `M1.md`.
- **Mostly fixed 2026-10-17/18**: parallel GPU tests used to hang on NVIDIA/Windows when Vulkan and
  GL were both enabled; tests now use `atd_gpu::primary_instance()` (DX12 on Windows, A24), and
  `.cargo/config.toml` caps test threads at 4 because 16 threads still stalled about 1 run in 20.
  Cause unknown. Unfixed:
  `midi::opening_without_devices_...` missed its deadline twice after long idle periods (not
  reproducible; see the M1 log). Re-run it before suspecting a real regression.

## Local set-up

- Rust 1.97 (`rust-toolchain` is pinned in CI), Python **3.13** on `PATH` (the app, CLI and
  `atd-py` embed it through PyO3), and for the AI Host `uv`.
- Linux only: `libasound2-dev libudev-dev`.
- FFmpeg development files only for `--features atd-io/ffmpeg`.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
cargo run -p atd-cli -- docs --check
cargo run -p atd-cli -- check --gpu examples/*.atidarla
cargo run -p atd-app -- examples/audio-reactive-feedback.atidarla
cargo test -p atd-io --features ffmpeg --test ffmpeg -- --nocapture
```

Follow `CLAUDE.md` (binding rules) and keep logging changes with evidence in `M1.md`.
