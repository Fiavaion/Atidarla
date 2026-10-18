# M2 pre-work: Packs and nested networks (FR-P4)

Written 2026-10-18 by Claude (one of five parallel M2 agents), on Mark's authorisation to run
ahead of "one milestone at a time". Nothing here is ratified. Questions and proposed
decision-log rows are at the end. Reference for the model and the format: [`docs/packs.md`](../../packs.md).

## What was built

| Task (brief) | State | Where |
|---|---|---|
| 1. Core model: nested network node, promoted parameters, sockets by wire type, validation (types, cycles through nesting, feedback inputs), cook semantics, undo/redo, help, semver, lock/unlock, update in place | **Done** | `crates/atd-core/src/pack.rs` |
| 2. Format: Pack folders in projects, Pack libraries by name+version, canonical form, round-trip/diff/fuzz tests | **Done** | `crates/atd-format/src/pack.rs` |
| 3. CLI: `check` (errors with file names), `fmt`, `render` and `check --gpu` cook projects with Packs | **Done** (also `check`/`fmt` of a Pack folder) | `crates/atd-cli/src/project.rs`, `render.rs` |
| 4. ≥5 useful Packs with help, promoted parameters, a version and an example each | **Done: 7** | `packs/`, `examples/pack-*.atidarla` |
| 5. App UI: Make Pack, Palette, enter/leave, version indicator | **Not done.** First step only: the app opens and plays projects with Packs (expanded, saving refused) | `crates/atd-app/src/project.rs` |

### Decisions taken inside the slice (smallest version; for review)

- **A Pack instance is an operator** of type `cmp.pack_<name>` (CMP family, PRD §6 lists "Pack"
  there) whose parameters are the promoted parameters. `PackSet::register` adds one schema per
  Pack to a registry, so every existing mechanism (typed wires, cycle checks, bindings,
  expressions, `Edit`/`History`, the network file) works for instances with no new code.
- **Flatten before cooking**, not "cook as a unit": the engine gets one flat network, so the
  scheduler, caching, feedback, bindings, both expression tiers, SIG/IMG mixing and offline mode
  are reused unchanged. Flattening runs on load and edit, not per frame. Inner IDs are a hash of
  the instance path (bit 62 set), so they are stable across unrelated edits and the cook order is
  deterministic. Evidence below.
- **A nested network is an unlocked Pack** (no separate "Container" type). Make Pack creates an
  unlocked 0.1.0 Pack; library Packs are locked; unlocking an instance copies its Pack to
  `<name>_local<n>` with `derived_from = "name@version"`.
- **A project carries its own copy of each Pack** (`<project>/packs/<name>/`, the PRD §5.5
  layout), so projects are self-contained (FR-S9) and a library change never alters a project
  silently. Updating is explicit (`PackSet::update`) and reports what changed.
- **Pack folder** = `pack.toml` (interface and metadata) + `network.toml` (the inner network in
  the `networks/*.toml` form, so its diffs read the same) + `help.md`. Libraries may keep older
  versions as `name@x.y.z/`.
- **Update rule** for instance values: keep if valid, clamp numbers into a new range, otherwise
  reset to the new default and report the old value; drop wires to removed or retyped sockets.
- **Limits**: depth 16, names ≤ 48 characters.

## Evidence (run on the RTX 5070 Ti PC, Windows 11, DX12, 2026-10-18)

All on branch `worktree-agent-a6791b072ba289c09` (based on `claude/compassionate-franklin-c9opw6`
at `f30b841`), with four other agents building and running GPU tests on the same machine.

| Check | Result |
|---|---|
| `cargo test --workspace` | **323 passed, 0 failed, 1 ignored** (the ignored test is the existing one) |
| New tests | atd-core `pack::tests` 15 (versions; schema; 14 kinds of invalid definition; int/menu promotion; nesting at the depth limit, one deeper, 5000 deep (error, no stack overflow), cycles incl. self; wiring/cycles/ranges/undo of instances; flattening with stable IDs and deterministic planning; nested promotion and expression renaming; feedback through a Pack; lock/unlock/edit/relock; update with report; update inside other Packs; Make Pack gives back the original graph and one undo restores it; reference rewriting; 400 random Pack sets never panic) + `tests/pack_plan_alloc.rs` 1 (209-node flat network from 3-level nesting: 0 allocations over 100 plan+record frames, same order every frame). atd-format `pack_tests` 7 (canonical sample, one-line diffs, hand-written equivalents, errors, 199 random Packs round-trip byte-stable, 3000 mutated `pack.toml` never panic, project save/load incl. stale removal, libraries by version). atd-cli 3 (shipped Packs complete and example copies current; Pack errors name their file and `fmt` fixes canonical form; Pack examples render identically twice, and an expression inside a Pack and one naming a Pack's channel evaluate). atd-app 1 (a Pack project opens expanded, the viewer starts on the Pack output, it cooks, saving is refused and the files stay unchanged). |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` | clean |
| `atidarla docs --check` | ok (pages regenerated: the new examples use more operators) |
| `atidarla check --gpu examples/*.atidarla` | 18 ok (11 existing + 7 Pack examples), every node cooked 60 frames, every binding found its channel |
| `atidarla check --gpu packs/*/` | 7 ok (one instance of each, nothing connected) |
| `atidarla render` twice, frames 0–60, SHA-256 per frame | pack-feedback-trails 60/60 identical (60 distinct), pack-audio-pulse 60/60 (26 distinct), pack-kaleido-tunnel 60/60 (60 distinct) |
| Looked at rendered frames | Chromatic Aberration, Feedback Trails, Neon Edges, Vignette, Beat Flash checked by eye |
| `atd-app -- examples/pack-kaleido-tunnel.atidarla --smoke 90 --screenshot` | ok: both tunnels expanded in the patch, viewer showing the composite |

Not checked: macOS (rule 6). Nothing in this slice is platform-specific, and CI's `check packs/*/`
step runs on both OSes, but it has not run yet.

## Shipped Packs (7 of the ≥15 the Palette needs)

| Pack | Built from | Promoted |
|---|---|---|
| Audio Pulse | Spectrum → Select → Lag → Math → bound Level + Bloom; SIG output `pulse` | gain, attack, release, quiet, loud |
| Beat Flash | Beat → Select → Trigger → Math → bound Level; SIG output `flash` | bpm, decay, intensity, restart |
| Chromatic Aberration | 3 Channel Mix, 2 Displace (one through an expression), 2 Composite | amount, direction |
| Feedback Trails | Feedback, Transform, Level, HSV Adjust, Composite loop | decay, zoom, spin, hue_shift, blend, reset |
| Kaleidoscope Tunnel | the kaleidoscope-tunnel example as a generator | speed, segments, scale, repeat, hue_speed, glow |
| Neon Edges | Edge → Blur → Level → HSV Adjust → Composite (screen) | strength, width, glow, hue, saturation, mix |
| Vignette | Ramp (radial) → Level → Composite (multiply) | amount, softness, size, colour |

**Plan for the other 8 or more** (all from existing IMG/SIG operators unless noted): Film Grain
(Noise with `seed` following `frame`, composite), Colour Grade (Level + HSV Adjust + Lookup with a
Ramp), RGB Delay (three Caches, one per channel), Slit Scan (Time Machine fed a Ramp), Glitch
Blocks (Tile + Threshold-masked Transform switching on a Trigger), Ripple Touch (Ripple driven by
the mouse from Input Devices), Reaction Bloom (Reaction-Diffusion seeded by the input, Bloom),
Fluid Smoke (Fluid 2D with an audio-driven emitter), Posterise/Threshold Duotone, LFO Bank (SIG
only: four LFOs merged, useful as a modulation source). After A12 (Kernel DSL) and the geometry
work: particle and mesh Packs (curl-noise emitter, instancer). A Palette of 15 is reachable with
operators that exist today.

## Not done, and the next step for the app

The app UI (brief task 5) is not built beyond opening Pack projects read-only. **Why:** the
engine (`atd-gpu`, another agent's area) holds the network the app edits and cooks that same
network. Editing with Packs needs the engine to keep the **edited** network (with instances; for
`Edit`, history and saving) and cook a **flattened copy** of it. Changing the engine was outside
this slice, and so was rewriting the app's ~50 edit paths around a wrapper.

Proposed design (about a day; needs Mark's yes, see Q1):

1. `Engine::set_packs(PackSet)`: registers the Pack schemas in the engine registry; the engine
   keeps `net` (edited) and a `Flat` (cooked). After each edit, undo or redo, it flattens again
   and marks dirty the flat nodes whose node, parameters or inputs changed (a diff of the two
   flat networks). Operator state survives because inner IDs are stable.
2. `result`/`channels`/`error`/`stat` take edited-network IDs and map instances through
   `Flat::view`. Inner nodes are reachable by flat ID for "enter Pack".
3. App: **Make Pack** (selection → `make_pack`, a dialog choosing the parameters to promote,
   one undo step); **Palette** list from `Library::open(<repo or install>/packs)` with
   label, summary and help, inserting copies the definition into the project; **enter/leave**
   a Pack shows `def.network` in the canvas (read-only with a lock badge when locked, edits
   through `PackSet::edit` when not; "Unlock" button calling `PackSet::unlock`); **version
   indicator** on an instance when the library has a newer version (`compatible_with` decides
   "update" or "breaking update"), with the `UpdateReport` shown before applying.
4. Remove the read-only guard in `ProjectState::save_to`.

Also not done: a Pack's own `cook_budget_us` is the sum of its inner budgets (no measured budget
or bench case per Pack); the docs generator does not give Packs pages (their `help.md` is the
page); Packs in `atidarla bench` (FR-H4).

## Questions for Mark

1. **Engine change for the app** (the design above): may the engine own "edited network + flat
   copy"? It touches `atd-gpu/src/engine.rs`, which the geometry agent is also changing; it would
   be a new A25-style extension point.
2. **Type IDs** `cmp.pack_<name>` (instances are CMP operators): fine, or a separate namespace
   such as `pack.<name>` (which would need a ninth prefix outside A10's eight families)?
3. **Flatten vs cook-as-unit**: I chose flattening (reasons above). Agree?
4. **Projects carry copies of their Packs** and update only on request (never automatically from
   the library). Agree? Should `project.toml` also list the Packs and versions it uses (it is
   implied by `packs/` today)?
5. **No separate Container type**: a nested network is an unlocked Pack. Agree, or should
   Container be its own CMP operator (an inline sub-network without a `packs/` folder)?
6. **Expressions**: in a promoted expression `me` is the inner node it drives, and
   `op('instance').par.<promoted>` does not resolve. Acceptable for now?
7. **Examples**: one example project per Pack, each carrying a copy (a test keeps it equal to
   `packs/`). Fine, or one shared Pack showcase project?
8. **Library location** for installed builds (the repo's `packs/` in development): beside the
   executable, or a user folder (both)? Needed for the Palette.

## Proposed decision-log rows (for Mark to accept, amend or reject)

| # | Decision | Status | Notes |
|---|---|---|---|
| A26 | **Packs (FR-P4):** a Pack is a network with promoted parameters, sockets by wire type, a semantic version, help and a lock flag; an instance is an operator `cmp.pack_<name>` whose parameters are the promoted ones; a nested network is an unlocked Pack; the engine cooks a network **flattened** on load and edit (inner IDs derived from the instance path), never a Pack as a unit | Proposed 2026-10-18 | `docs/packs.md`, this file |
| A27 | **Pack files:** `packs/<name>/pack.toml` + `network.toml` + `help.md`, in projects (each project carries its own copies) and in libraries (`name@version/` for older versions); updating a project's Pack is explicit and reports interface changes and the keep/clamp/reset of each instance's values | Proposed 2026-10-18 | A11, rule 8 |

## For the other areas

- **Anything that loads a project into an engine must flatten first** (`project.packs.register`,
  then `flatten`, then `Engine::load(flat.network)`). The CLI and app do. The engine never sees
  `cmp.pack_*` types, so no `OpLibrary` entry is needed for them. `classify()` returns `Pure` for
  the prefix only so the classification test can cover them.
- `atd_format::Project` has a new `packs: PackSet` field. Build projects with `Project::new` or
  `with_main`.
- Pack schemas are per project (like ISF shaders). Do not hard-code registry counts that include
  them. `builtin_registry()` is unchanged.
- `crates/atd-cli/src/project.rs` was restructured (`load_checked`, `cook`, `check_pack`), and
  `render.rs` `Renderer::load` now flattens; both files are LF now (rustfmt normalised them).
  Expect conflicts there; keep the flatten step when merging.
- `docs/operators/*` were regenerated because the new examples use more operators; regenerate
  after merging (`atidarla docs`).
- CI has a new step, `check packs/*/`.
- PNT/MSH, Kernel and Python operators will work inside Packs with no Pack changes, as long as
  they are ordinary registry operators: promoted parameters take any parameter kind, and sockets
  any wire type (GEO included).
