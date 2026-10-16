# Handoff: continuing M1 locally

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

## In progress: FFmpeg video (A23)

Committed but **not finished or in CI yet**:

- `crates/atd-io/src/video/ffmpeg_shim.c`: a C shim over FFmpeg (open, frame times by
  scanning packets, frame by index with keyframe seeking, RGBA via swscale with the stream's
  colour matrix, hardware decode via VideoToolbox / D3D11VA / VAAPI with software fallback).
- `crates/atd-io/build.rs`: compiles the shim when the `ffmpeg` feature is on; finds FFmpeg
  with pkg-config, or `FFMPEG_DIR` (`include/`, `lib/`) on Windows.
- `crates/atd-io/src/video/ffmpeg.rs` (Rust FFI) and `run_other` in `video/mod.rs`: Movie In
  plays non-HAP files through it. Feature chain: `atd-app`/`atd-cli` `ffmpeg` →
  `atd-ops-img/ffmpeg` → `atd-io/ffmpeg`. Off by default.
- `crates/atd-io/tests/ffmpeg.rs` with H.264 (B-frames), VP9 (WebM) and ProRes clips in
  `tests/data/`: frames in any order match FFmpeg's own decode (≤ 8 levels), the player
  works end to end, and damaged files do not crash. **Passed locally on Linux with FFmpeg 6.1
  (software decode).**

Still to do for A23:

1. **CI**: install FFmpeg development files and build/test with
   `--features atd-io/ffmpeg` on each OS:
   - Linux: `apt-get install libavcodec-dev libavformat-dev libavutil-dev libswscale-dev`.
   - macOS: `brew install ffmpeg pkgconf` (a GPL build; fine for testing only, A23 ships LGPL).
   - Windows: an **LGPL shared** build (for example BtbN FFmpeg-Builds `*-win64-lgpl-shared-*`),
     set `FFMPEG_DIR` to it and put its `bin/` on `PATH` for the DLLs.
2. Run the tests on a Mac and a Windows PC: does hardware decode engage (the test prints
   `via videotoolbox` / `via d3d11va`) and do frames still match?
3. Add a log row and update the Movie In line in `docs/milestones/M1.md`.
4. Packaging (M6): ship FFmpeg's LGPL DLLs/dylibs beside the app, replaceable (A23).

## Other open M1 items

- **AC1 by hand**: build the audio-reactive feedback patch in the app on a Windows and a Mac
  machine, save, reload, compare (record in M1.md).
- **AC3 / PT-1**: `cargo run -p atd-cli --release -- bench` on the RTX 4070 PC.
- **Share In/Out**: Spout (Windows) and Syphon (macOS) wire-up, on real machines.
- Check CI for the last pushes (runs 71 onwards). Run 70 failed only on a Windows path test,
  fixed in `5200807`.

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
