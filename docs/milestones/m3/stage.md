# M3 slice: Stage core

**Date:** 2026-10-30. **Covers:** FR-S1 (launch modes, column launch, crossfader A/B), FR-S3 (blend modes, per-layer transitions), FR-S8 (autopilot), PT-7 groundwork. **Architecture:** decision A54, `../M3.md`.

## What was built

| Item | Where | Notes |
|---|---|---|
| `img.layer` | `crates/lsr-ops-img/src/stage.rs` | IMG inputs `clip0`…`clip15`, optional SIG `clock`. Params `target` (−1 none), `transition` (cut/fade/wipe), `transition_length` + `transition_unit` (beats/seconds), `quantise` (off/beat/bar), `opacity`, `blend`, `bypass`, `slots`. State (`Switcher`, pure and unit-tested): the playing slot, the outgoing slot and progress of a transition, and a launch queued with the quantum count it was queued at. It switches when the clock's `beat` or `bar` count changes, or in the same cook when quantise is off or there is no clock. Status line `playing 2 · queued 3`. One full-screen pass with two inputs (outgoing, incoming); the output comes from the engine's pool; the status string is rewritten only when the state changes |
| `img.mixer` | same file | IMG inputs `layer0`…`layer15`, per layer `opacity{i}`, `blend{i}`, `group{i}` (none/a/b); `crossfader`, `master`, `stage` (the deck the Stage tab shows), `autopilot`, `autopilot_after`, `autopilot_beats`, `autopilot_column`. One pass samples all 16 inputs (bottom to top); the blend and the effective opacity (opacity × crossfader weight) are packed per layer as `2 × blend + opacity` in the uniform block |
| Shared blend modes | `crates/lsr-ops-img/src/ops.rs` (`COMPOSITE_WGSL`) | Composite's nine operations moved into one WGSL function used by Composite, Layer and Mixer; Composite's tests unchanged and passing |
| Stage model + launch logic | `crates/lsr-stage/src/lib.rs` | `Stage::read`: decks = mixers, layers = `img.layer` reached from a mixer input (also through single-input effects, up to 8 hops), clips = nodes in slots, columns = slot indices. `Launcher` returns `Edit`s: trigger, toggle, flash press/release, one-shot (back after the clip's `beats`, default 4), column launch (one undo step; layers without a clip there stop; layers whose `target` is bound or has an expression are skipped), stop, stop all; `tick(deck, beat)` returns one-shots and runs the autopilot (next / seeded random never repeating / specific column, after N beats or at the column's longest clip length). `parse_slots`/`format_slots`, `LayerStatus::parse` |
| Stage tab | `crates/lsr-app/src/stage.rs`; `app.rs` +6 lines | Grid of layers (top layer first) × columns: clip labels, playing/queued marked by text (`>`, `...`) and colour, column buttons, per-layer Stop, Stop all, crossfader and master sliders (one undo step per drag), autopilot state, beat position. Click launches by the clip's mode; flash plays while held. The app cooks the Stage deck every frame as well as the viewed node and sinks (`StagePanel::cook_frame` replaces `Engine::cook_frame`) and ticks the launcher first |
| Example | `examples/stage-deck.lasair` | 3 layers × 4 columns of generator chains, `sig.beat` at 120 BPM: `backs` fades over 2 beats quantised to the bar, `shapes` wipes quantised to the beat and follows an LFO (so it switches by itself in a render), `accents` shows toggle, flash, one-shot and trigger |
| Offline classes | `crates/lsr-cli/src/render.rs`, `docs/render.md` | Layer: Stateful; Mixer: Pure |
| Bench | `crates/lsr-cli/src/bench.rs` `pt7.launch_cut` | Frames from the `target` edit to the new clip in the layer's output (cut, quantise off, 8 clips cooking, 1920×1080) |

## Evidence (run on Apple M5 Pro, Metal, 2026-10-30)

- `cargo test -p lsr-ops-img --test stage`: `layer_cuts_in_the_launch_frame`, `layer_fades_and_wipes_over_time` (half-way fade pixel, wipe halves, no cook requested after the fade), `quantised_launch_switches_on_the_clock_boundary` (scripted clock from `sig.constant`: waits within the beat, switches in the frame the beat count changes; bar quantise ignores beats), `mixer_composites_layers_through_the_crossfader` (groups at 0, 1 and 0.5, add blend, opacity, master).
- `cargo test -p lsr-ops-img --lib stage`: six `Switcher`/crossfader/status unit tests. `cargo test -p lsr-stage`: 10 tests (model reading through an effect, marked deck, slots parse/round trip, status parse, trigger/toggle/stop, flash and one-shot, column launch, autopilot next/column/random replay/clip end, channel-driven targets).
- `every_img_operator_steady_state_allocations` (`tests/alloc.rs`) covers both operators within the default bound.
- Offline render determinism: `cargo run -p lsr-cli --release -- render examples/stage-deck.lasair --out .scratch/a --range 0-120` twice (to `a` and `b`): `diff -rq` reports no difference over 120 frames; frames change (a mid-wipe is visible at frame 100). Also as a test: `two_renders_of_the_stage_deck_are_identical`.
- Round trip: `examples_are_valid_and_canonical` (load → save is byte-identical) includes the new example; `every_img_and_sig_operator_has_an_example` passes with it.
- `cargo run -p lsr-cli -- check --gpu examples/*.lasair`: all 45 ok, `stage-deck` 23 nodes.
- App: `cargo run -p lsr-app -- examples/stage-deck.lasair --smoke 90 --screenshot …` with the Stage tab temporarily in front showed the grid with playing clips, `shapes` with a queued clip, and the deck in the viewer.
- **PT-7 (indicative):** `pt7.launch_cut` = **1 frame** worst case over 20 launches (the cook right after the edit), launch frame p50 **6.15 ms** including GPU completion, 1920×1080, M5 Pro, release build. HAP/H.264 numbers need Movie In clips and the RTX 4070.

## Not done / follow-ups

- **Thumbnails** (FR-S2 "live-updating"): the app's only texture helper (`viewer::TextureViewer`) is one display pass per instance and needs the eframe renderer, which the tab UI does not get. A thumbnail pool (one small display texture per visible clip, registered once) is a follow-up.
- ~~All wired clips cook every frame.~~ Done 2026-10-30: operators name the inputs they read (`GpuOp::needed_inputs`), and a layer cooks only the playing clip, the outgoing one in a transition and the target. Clips not shown do not cook, so a stateful clip (feedback, a simulation, a movie) pauses while hidden. A `target` bound to a channel or driven by an expression reads every clip (its value is only known after the plan). Preroll of neighbouring columns is not done.
- Legato and layer groups (FR-S1) are not implemented.
- Launching a clip on a layer whose `target` is bound to a channel replaces the binding (the tooltip says so); column launch skips such layers.
- Autopilot, one-shot returns and flash releases are app behaviour: they write edits (undo steps) and do not run in `lasair render`.

## Open questions for Mark

1. **Layer `blend` vs Mixer `blend{i}`.** M3.md gives both a blend. Blending onto the layers below needs the layers below, so it lives on the Mixer; the Layer's `blend` is used as the *transition blend* (a fade passes outgoing → incoming blended onto outgoing → incoming). Keep, rename to `transition_blend`, or drop?
2. **Crossfader curve:** both groups at full in the middle (A = min(1, 2(1−x)), B = min(1, 2x)). Linear instead?
3. **Column launch on empty cells** stops that layer (scene behaviour). Leave such layers playing instead?
4. **Clip settings** live in the layer's `slots` text (`slot,mode,beats,label`), as M3.md suggests. Fine as text, or per-slot parameters?
5. ~~The Stage deck cooks every frame in the app~~ Resolved 2026-10-30: `Engine::cook_frame_with(viewed, extra, time)` cooks the deck's mixer with the viewed node and every sink; the duplicated loop is gone.
