# M3 slice: DMX fixtures and pixel mapping, show export, recording

**Date:** 2026-10-30. **Covers:** FR-S7 (fixture library, LED pixel mapping from IMG regions),
FR-S9 (show export), FR-S10 (record composition output to file); groundwork for M3 AC1 ("DMX to
a test fixture") and AC2 (bundle exported on one OS plays on the other). **Decisions used:** A23,
A51 (LGPL FFmpeg, LGPL encoders only), A19 (capability flags), A54 (`../M3.md`).

## What was built

| Item | Where | Notes |
|---|---|---|
| Fixture types and patch files | `crates/atd-io/src/fixture.rs` | TOML. A fixture type lists one pixel's channels by role (`dimmer`, `red`, `green`, `blue`, `white`, `amber`, `pan`, `tilt`, `open`, `none`); ten built-ins (`dimmer`, `rgb`, `drgb`, `rgbw`, `drgbw`, `rgbwa`, `pixel-rgb`, `pixel-grb`, `pixel-rgbw`, `moving-head`); project types in `fixtures/<id>.toml` win over built-ins. A patch places fixtures (`id`, `type`, `universe`, `address`, `count`) on a `point`, `line` or `rect` (+ `grid`, `snake`). `Mapping` lays it out once: sample cells for the GPU and a flat list of slot writes, so a frame fills universes in one loop without allocating. Pixels never straddle universes (171 RGB pixels: 170 + 1); double-patched slots and bad keys are errors naming the fixture |
| `img.pixel_map` (Pixel Map) | `crates/atd-ops-img/src/pixel_map.rs` | IMG in, SIG out, sink. Patch read on a watcher thread (reloads on save). One compute pass averages 4 × 4 texels per cell; three readback buffers mapped after submit, read on a later cook (Analyze's pattern): one or two frames late live, exactly one offline. Channels `u<universe>_<slot>` (0–1); `universe` limits them to one universe for DMX Out; `protocol` `artnet`/`sacn` sends every patched universe itself through one `DmxSender` thread per universe. Offline: classed as a sink, sends nothing |
| `atidarla export-show` / `verify-show` | `crates/atd-cli/src/export.rs`, `main.rs` | Path parameters found from the registry (`ParamKind::Path`) in every network and inside Packs; external files copied to `media/` (deduplicated by content, `name-2.ext` on a clash, `.gltf` with its buffers in `media/<stem>/`); Windows separators made portable; Mesh File Out targets rewritten to `out/`, not copied; missing files stop the export with a list. Writes the copy with `atd_format::save` (canonical), `bundle.toml` (every file's SHA-256 and size, `external` flag, `[[node]]` per capability flag with its fallback, `[models]` placeholder), then verifies (relative exact-case paths, hashes, `check --gpu`). `--zip`: sorted entries, fixed timestamps, media stored |
| Encoding shim | `crates/atd-io/src/video/ffmpeg_encode.c`, `build.rs` (one more file in the same `cc` build; non-ffmpeg builds unchanged) | RGBA → BT.709 limited-range YUV via `sws_scale_frame` with tagged frames; `prores_ks` (profiles 2/3/4, 16-bit alpha for 4444), hardware H.264/H.265 (NV12 preferred), `hvc1` tag, GOP 1 s, no B-frames |
| `Recorder` | `crates/atd-io/src/video/record.rs`, `ffmpeg.rs` (`Encoder`) | Encoder on a worker thread, four frame buffers allocated at start; `try_frame`/`submit` (live: never waits, drops and counts), `push_wait` (offline: every frame). Codec → encoder names: `prores_ks`; `h264_videotoolbox`/`hevc_videotoolbox` (macOS); `h264_nvenc`, `h264_mf` / `hevc_nvenc`, `hevc_mf` (Windows); never `lib*`. Without the feature `start` explains how to rebuild |
| `atidarla render --out *.mov|*.mp4` | `crates/atd-cli/src/render.rs` | `--codec prores|prores-hq|prores4444|h264|h265`, `--bitrate`. Default build refuses with the rebuild command. PNG path unchanged |
| Record in the app | `crates/atd-app/src/record.rs`; `app.rs` (+12 lines) | Viewer toolbar **Record**/**Stop** with status (seconds, frames, dropped); file dialog and file closing on worker threads; `Capture`: ring of three readback buffers, `map_async`, polled without waiting. API: `RecordPanel::start(path)`, `frame(device, queue, texture)`, `stop()` |
| Example | `examples/pixel-map.atidarla` | Noise → Pixel Map (strip, PARs incl. a project fixture type, serpentine grid on universe 2) → DMX Out (universe 1, 127.0.0.1) |
| Docs | `docs/dmx.md`, `docs/show-export.md`, `docs/render.md` (movie files, live recording), `docs/operators/img/pixel_map.md` (generated) | |

### Why `img.pixel_map`, not `sig.pixel_out`

PRD §6 lists "Pixel Out (points → DMX/LED/laser)"; FR-S7 asks for "LED pixel mapping from IMG
regions". The operator reads an image and outputs channels, as Analyze does, so it is an IMG
operator named for what it does. A points-driven `pnt.pixel_out` (sample positions from a PNT
stream, lasers) can reuse `atd_io::fixture` later. Pixel Map is classed as a **sink** (it sends
when `protocol` is set, so it must cook with nothing downstream); the render test requires sinks
to be classified as such.

## Evidence (Apple M5 Pro, Metal, macOS, Homebrew FFmpeg 8 / libavcodec 63.1, 2026-10-30)

- **Pixel map:** `cargo test -p atd-ops-img --test pixel_map`: `gradient_gives_the_expected_dmx_channels` (img.ramp 64×16 → strip of 8 RGB pixels + a project fixture → 26 slots equal to a CPU reference of the same 4 × 4 average within ±1, rising left to right), `loopback_fixture_receives_artnet_and_sacn` (a UDP socket on 127.0.0.1 parses ArtDmx and E1.31 packets as a fixture would; universe 1, slot bytes match the reference within ±1), `patch_errors_are_node_errors`. `cargo test -p atd-io --lib fixture`: 5 tests (roles and white extraction, layout and serpentine cells, universe wrap at 170 pixels, overlaps, project fixture override, unsafe type IDs).
- `check --gpu examples/pixel-map.atidarla`: ok, 3 nodes.
- **Show export:** `cargo test -p atd-cli export`: 4 tests. A project naming three files outside it by absolute path (two different `clip.mov`, one named twice) and one inside with Windows separators exports to `media/clip.mov`, `media/clip-2.mov`, `media/still.png`, `media/inside.jpg`; hashes equal the sources'; the bundle is moved elsewhere, the originals deleted, and it still verifies and loads; a changed file is caught; missing files and an output folder inside the project are refused; the pixel-map example exports with its patch and fixtures and verifies with `--gpu`.
- CLI run: the audio-reactive-feedback example with its WAV moved outside and named by absolute path: `export-show … --zip` → 4 files, 1 external, `verify` with `--gpu` ok (30 frames); `render --range 0-20` of the original and of the bundle: all 20 PNGs byte-identical. `export-show examples/texture-sharing.atidarla` lists `share_out1`/`share_in1` with `needs = "win|mac"`, `fallback = "hide"`.
- **Record:** `cargo test -p atd-io --features ffmpeg --test record` (30 frames of 128×64 gradients, decoded back with the existing FFmpeg decoder, worst frame):

  | Codec | Encoder | Mean abs diff | Max | Bound in the test |
  |---|---|---|---|---|
  | ProRes 422 | prores_ks | 1.85 | 5 | mean < 2.5, max ≤ 8 (FFmpeg CLI round trip of the same frames: 1.71, 5) |
  | ProRes 4444 (RGBA) | prores_ks | 0.15 | 1 | same |
  | H.264 8 Mbit/s | h264_videotoolbox | 1.26 | 7 | mean < 3, max ≤ 24 |
  | H.265 8 Mbit/s | hevc_videotoolbox | 1.26 | 7 | same |

  Frame count (30), size and times (i/30 s) checked; odd sizes, ProRes in `.mp4` and a missing folder fail with messages.
- `atidarla render examples/stage-deck.atidarla --out deck.mov --range 0-120` (ffmpeg build): 120 frames, `prores_ks`, ffprobe: prores Standard, yuv422p10le, bt709, 60/1, 120 frames. `--out deck.mp4 --codec h265`: hevc_videotoolbox, tag `hvc1`, 120 frames. Default build: refused with "built without FFmpeg. Rebuild with `cargo build -p atd-cli --features atd-io/ffmpeg` …".
- App: `cargo test -p atd-app record`: `capture_reads_frames_back_in_order` (no waits on the caller's side; skipped frames counted), `record_panel_writes_a_movie` (ffmpeg build: start, 40 frames, stop, file closed on its worker, ≥ 30 frames reported).
- **HAP:** not available: Homebrew's FFmpeg lists no `hap` encoder (`ffmpeg -encoders`; it needs `--enable-libsnappy`). See open question 2.

**Gate (this worktree, 2026-10-30):** `cargo build --workspace` ok; `cargo test --workspace --no-fail-fast` 570 passed, 0 failed, 9 ignored (a first fail-fast run hit `atd-ops-pnt` `geo_chains_steady_state_allocations` under parallel load, 22 vs 20 buffers; it passes alone and in the full rerun; untouched by this slice); `cargo clippy --workspace --all-targets -- -D warnings` clean; with `--features atd-io/ffmpeg`: build ok, `cargo test -p atd-io -p atd-cli` 76 passed, clippy (`atd-io`, `atd-cli`, `atd-app`) clean; `cargo fmt --all -- --check` clean; `cargo deny check` ok (zip 9: zip 8 has RUSTSEC-2026-0367/0368); `atidarla docs --check` ok; `check --gpu examples/*.atidarla` 46 of 46 ok.

## Not done / follow-ups

- **HAP recording** (FR-S10 lists HAP): no encoder in the usual LGPL FFmpeg builds; a native encoder is the follow-up (question 2).
- **16-bit DMX channels** (`pan_fine`, `dimmer_fine`), colour temperature, amber derivation, and RDM are not in the fixture format yet. Gamma/brightness curves per fixture are not applied (values are the image's 8-bit levels).
- **Pixel Map sampling** averages 4 × 4 texels per cell: a large cell over a detailed image is sampled, not fully averaged (documented; scale the image down first).
- **Paths inside files** (Compute `#bind` files, Python code that opens files) are not rewritten by `export-show`; they work when relative to the project (documented).
- **Live recording timing:** frames are written at a fixed 60 fps, one per app frame; a slower app makes a faster-than-real-time movie. Audio is not recorded.
- The Windows → macOS run of AC2 needs the Windows PC (steps in `docs/show-export.md`); the encoder names `h264_nvenc`/`h264_mf`/`hevc_*` are compiled and tried in order but only ran where present (not on this Mac).
- Record API is in the app crate (`RecordPanel`); the Stage tab could add its own button with the same three calls (left to the Stage slice's owner).

## Open questions for Mark

1. **Pixel Map naming:** `img.pixel_map` now, `pnt.pixel_out` (PRD "Pixel Out") later for points/lasers, sharing the fixture library. Agree?
2. **HAP recording:** write a native HAP encoder (DXT1/DXT5 block compression + Snappy, BSD; both directions of the format already live in `atd-io` for decoding), or require an FFmpeg build with `--enable-libsnappy`? Native keeps "HAP stays native" (A23) and works with any FFmpeg.
3. **ProRes default profile:** Standard (smaller files) or HQ for `.mov`?
4. **Bundle zip and very large media:** zips over 4 GB use Zip64, which old unzip tools mishandle. Keep the zip optional (folder is the bundle), as now?
