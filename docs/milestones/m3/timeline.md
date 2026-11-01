# M3 slice: Timeline (FR-T1–T3)

**Date:** 2026-10-30. **Architecture:** decision A54 (`../M3.md`). **User reference:**
[`docs/timeline.md`](../../timeline.md).

## Built

| Part | Where | What |
|---|---|---|
| File format | `crates/lsr-format/src/timeline.rs` | `timeline.toml`: tracks per (node ID, parameter, component), keys (time, value, step/linear/bezier, `in`/`out` handles), markers, cues (time or manual, `when` condition, `set`/`live`/`launch` actions), length, fps, loop region. Canonical order and one line per key and per action. `Project::timeline` (`None` without the file: the project loads unchanged; saving `None` removes the file) |
| Curves | `crates/lsr-timeline/src/curve.rs` | Step, linear, cubic Bezier: the curve's time is solved by Newton steps in a bracket. Handles are clamped in time so the curve stays a function of time. Default handles give smoothstep |
| Evaluation | `crates/lsr-timeline/src/eval.rs` | `Timeline::bind` resolves nodes, parameter kinds and condition references once. `cross`/`finish`/`evaluate`/`chase`/`rearm` then allocate nothing: per-parameter slots with preallocated buffers, vectors written in place, cue-held values referenced, not cloned. Timed cues fire exactly once going forward; scrubs fire nothing; `chase(t)` fires everything before `t`; loops re-arm. Conditions use `lsr_core::expr` (native subset) |
| Transport | `crates/lsr-timeline/src/transport.rs` | Play, pause, stop, loop, seek/scrub, length end. `Motion` (`Still`/`Forward`/`Wrapped`/`Jumped`) tells the timeline what was crossed. Clock: internal or timecode |
| Chase | `crates/lsr-timeline/src/chase.rs` | `Chase` (lock after 2 consistent readings, 1-frame tolerance, 3-frame dropout, 12-frame freewheel by default, immediate re-lock when the code resumes in place, offset). `MtcInput` and `LtcInput` wrap `lsr_io::timecode` decoders |
| Engine glue | `crates/lsr-timeline/src/player.rs` | `Player`: ticks the transport, fires cues (live: `set`/`launch` as one edit per cue; offline: all actions held as live values), applies changed values only via `Engine::set_live`, clears on stop or when a track goes, re-binds when the document or network changes. `watched()` lists nodes conditions read (and the chased Timecode In node) so they cook with the frame |
| Offline render | `crates/lsr-cli/src/render.rs` | `Player::offline` evaluated at `i / fps` before each frame's cook. Watched nodes cook with the output. Timeline problems become render warnings |
| Check | `crates/lsr-cli/src/project.rs` | `lasair check` requires `timeline.toml` to be canonical and every track and cue to bind |
| App | `crates/lsr-app/src/timeline.rs` | Timeline tab: transport, timecode display, loop and length, Drive/Follow Stage, clock (internal or chase a Timecode In node, with state and offset), key at playhead from the selected node's parameter, markers, cues, ruler scrub, tracks with curves, key select/drag/delete, interpolation menu, key inspector with Bezier handles, cue list with status and actions (set, live, launch), marker list |
| App wiring | `app.rs`, `project.rs`, `main.rs` | One panel field; `TimelinePanel::frame` before the cook; `TimelinePanel::cook` replaces `cook_frame` (adds watched nodes); the tab; `ProjectState::dirty` includes timeline edits |
| Example | `examples/timeline-show.lasair` | Tracks animating noise z (linear), kaleidoscope rotation (Bezier) and segments (step), two colour components, mix and brightness. Four markers. Cues: conditional on an unwired LFO, `live`, `set`, and a manual cue |

## Evidence (2026-10-30, Apple M5 Pro, Metal)

| Claim | Test |
|---|---|
| Canonical form, round trip of 300 random timelines, one-line diffs, errors with location, project save/load with and without the file | `cargo test -p lsr-format timeline` (7 tests) |
| Curves against known values (smoothstep, parametric midpoint, a dense parametric reference to 1e-7, linear handles = linear, clamped handles monotonic) | `cargo test -p lsr-timeline --lib curve` |
| Typed live values (float, int rounding, toggle, vector component over the stored value), binding problems reported, cues exactly once at 5 frame sizes, scrub/chase/rearm, conditions armed then fired, Go order, tracks over held values, follow-Stage | `cargo test -p lsr-timeline --test timeline` (8 tests) |
| No allocation while playing (1200 frames with loop wraps, seeks, Go, 50 cues of which 17 conditional, 5 tracks incl. vector components) | `cargo test -p lsr-timeline --test alloc` |
| MTC: lock 3.75 frames from the first quarter frame at 24, 25 and 30 fps; error < 1 frame; freewheel through a 10-frame dropout, re-lock on the first full frame after; > freewheel → stops; source jump re-locks in 3.75 frames with offset | `cargo test -p lsr-timeline --lib chase` |
| LTC (synthetic audio from `encode_ltc_frame`, 60 Hz blocks): lock 2.08 frames (25 fps, 48 kHz), 2.50 (30 fps, 48 kHz), 2.40 (24 fps, 44.1 kHz); error < 1 frame | same |
| Offline render with a track and a cue: identical twice, red follows the keys within 2/255, cue at 0.6 s visible from frame 36; frame 48 rendered alone matches; going back to frame 24 un-fires the cue; network untouched | `LSR_REQUIRE_GPU=1 cargo test -p lsr-cli timeline_tracks_and_cues_render_deterministically` |
| Condition on an unwired LFO fires offline at the predicted frame (7) | `cargo test -p lsr-cli timeline_conditions_read_unwired_signals` |
| Example renders byte-identically twice (64 frames at 2 fps); the conditional colour cue and the invert cue are visible | `lasair render examples/timeline-show.lasair --fps 2 --range 0-64` twice, `diff -r` |
| App opens the example; panel draws ruler, markers, cues, 7 tracks with curves | `cargo run -p lsr-app -- examples/timeline-show.lasair --smoke 60 --screenshot …` |

## Deviations

- **Conditions** use the existing native subset, so a channel is `op('beat1')['beat_pulse'] > 0`,
  not a bare `beat_pulse`. The subset has no free names.
- **Conditional timed cues do not hold the playhead** (ossia's interactive triggers pause time).
  They are armed at their time and fire on the first frame the condition holds, while time runs
  on. This is the same live and offline. A holding mode would make offline frame times depend on
  signals.
- **Offline, `set` and `launch` are held as live values**, not edits, so renders are a function of
  frame time (A54 calls a launch an edit; it still is one live).
- **Chase in the app reads a Timecode In node** (`time`, `locked`) rather than opening a second
  MIDI or audio device. The decoders run on that node's thread. `MtcInput`/`LtcInput` cover raw
  feeds.
- **A timecode jump repositions without firing** the cues it skipped. A scrub with `chase` exists
  (`Transport::seek(t, true)`) but the panel does not offer it yet.
- **No "keyframe" button in the Parameters panel.** The Timeline's own picker keys the selected
  node's parameter, so `params.rs` is unchanged.

## Open questions

1. **Vector live values allocate when they change.** `Timeline::evaluate` writes vectors in place,
   but `Engine::set_live` takes an owned `Value`, so an animated colour clones a small array per
   frame. Scalars allocate nothing end to end. An engine API that updates a live value in place
   (`LiveValues::get_mut` or `set_live_with`) would close this (lsr-gpu, not changed here).
2. **`Engine::load` keeps live values.** It does not clear them, so values on a node ID reused by
   the next project survive a load. The renderer and the panel clear theirs; other live-value users
   (mappings) may want `load` to clear.
3. **Cooking watched nodes.** `TimelinePanel::cook` repeats `Engine::cook_frame`'s rule (viewed +
   sinks) to add the timeline's nodes in one cook. An `Engine::cook_frame_with(viewed, extra, t)`
   would remove the repetition.
4. **Stage → Timeline recording** (from the M3 slice table) is not built: launches are observed
   for follow-Stage, not recorded as cues.
5. Tracks on Pack instance parameters (the flat network renames them) are skipped and reported.
