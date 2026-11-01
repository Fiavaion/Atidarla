# M3 slice: Control (MIDI/OSC mappings, learn, OSCQuery, PT-8)

**Status:** built 2026-10-30 for FR-S5 and AC4 (PT-8) of `../M3.md`, on decision A54 (mappings write live values). User reference: [`docs/mappings.md`](../../mappings.md).

## What was built

| # | Item | Where |
|---|---|---|
| 1 | **`mappings.toml`** in the project folder: source (MIDI CC / note / pitch bend with optional channel and case-insensitive device pattern, or OSC address + argument index), target (node by stable ID, parameter, component), input and output range, curve (linear, exponential with amount, step), mode (absolute, toggle, momentary, relative), pick-up; `[osc]` port and OSCQuery settings. Canonical form (sorted by ID, every field, one order), defaults for hand-written files, unknown keys rejected. No file = no mappings: older projects load unchanged, and saving a project without mappings writes no file (and removes a stale one) | `crates/lsr-format/src/mappings.rs`, `lib.rs` (`Project::mappings`, load/save) |
| 2 | **Mapping runtime.** I/O threads (one OSC socket, one thread per MIDI input via `midir`) hand messages to a `ControlHub`, which matches them against the mappings on that thread and keeps per mapping only the latest value, rising edges (toggle) and summed steps (encoders). Once per frame `MappingRuntime::update` takes the slots under one short lock and computes the values (range, curve, mode, pick-up, component merge, integer/boolean/menu conversion); the caller applies `writes()` with `Engine::set_live`. No queue grows behind the cook: many messages between two frames give one write, the latest. Allocation-free after warm-up for number, integer and toggle targets (colour/vector/menu values are new `serde_json` values when they change) | `crates/lsr-io/src/mapping.rs` |
| 3 | **Learn API**: `arm(target)` → the next MIDI control (CC, note-on, pitch bend) or numeric OSC message becomes a mapping (`take_learned` / `learn_into`, which picks output range and mode from the parameter kind); `cancel_learn`. **Monitor**: last 64 messages | same |
| 4 | **App.** Right-click a parameter label in the Parameters panel → Learn (per component for colours and vectors); dot marks mapped / armed parameters. **Mappings window** (View → Mappings): OSC port, OSCQuery switch and port, open MIDI inputs (rescan), every mapping editable (ranges, curve, mode, pick-up) and deletable, live input monitor. Mapping edits count as unsaved changes. Live input wakes an idle UI (`request_repaint` from the I/O thread). Opening another project clears live values | `crates/lsr-app/src/mappings.rs` (new), a few lines in `app.rs`, `params.rs`, `project.rs`, `main.rs` |
| 5 | **Save write-back** (documented choice): saving first stores the live value of every mapped parameter as its stored value (one undo step "Store mapped values"; bindings and expressions kept), so the show reopens as the controls left it | `mappings::write_back`, `ProjectState::save_to` |
| 6 | **OSCQuery**: std-only HTTP/1.1 GET server on its own thread: `/` namespace tree (container per node, entry per parameter with `FULL_PATH`, `TYPE`, `VALUE`, `RANGE`, `ACCESS`, `DESCRIPTION`), `/node/param`, `?ATTRIBUTE`, `?HOST_INFO`; mDNS `_oscjson._tcp` + `_osc._udp` via `mdns-sd`. OSC to `/<node>/<param>` sets the parameter as a live value (routes in the same hub). Values are fresh: a request asks the app (atomic flag + waker) to publish on its next frame and waits ≤ 100 ms; the app rebuilds routes only when nodes are added, removed or renamed (allocation-free fingerprint). Off by default; on per project (`[osc] oscquery = true`) or with `lasair-app --oscquery` | `crates/lsr-io/src/oscquery.rs` |
| 7 | **PT-8** test and bench case `pt8.osc_to_pixel` | `crates/lsr-cli/src/pt8.rs`, one case in `bench.rs` |

New crates: `mdns-sd` 0.21.6 (Apache-2.0 OR MIT), default features off except `logging`, bringing `flume` (Apache-2.0/MIT), `if-addrs` (MIT OR BSD-3-Clause), `socket-pktinfo` (MIT), `socket2` (MIT OR Apache-2.0), `mio` (MIT), `spin` (MIT), `fastrand` (Apache-2.0 OR MIT), `wasi` (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT). `cargo deny check`: advisories, bans, licences, sources ok. The HTTP server needs no crate.

## Evidence (actually run, 2026-10-30, Apple M5 Pro, macOS, Metal; other agents were building and testing on the same machine)

| Check | Result |
|---|---|
| PT-8 test `cargo test -p lsr-cli pt8` (debug build) | 20 trials at 60 fps, packet sent a third of the way into a frame: **1 frame** every trial (budget 2); send → pixel read back median 15.1 ms, max 17.4 ms (≈ 11 ms of that is waiting for the next frame boundary) |
| `cargo run --release -p lsr-cli -- bench` | `pt8.osc_to_pixel` ok: 20 trials, **max 1 frame** (budget 2), latency median 13.7 ms, max 14.6 ms. Same run: PT-1 p50 20.6 ms, PT-2 p50 9.8 ms, mesh 1M p50 3.9 ms |
| Format tests | random mappings round-trip exactly and byte-stable (300 cases), canonical text pinned, one-field edit = one-line diff, hand-written defaults, errors name the mapping, 2000 mutated files never panic, project folder with/without `mappings.toml` |
| Runtime tests (`lsr-io` `mapping::tests`, 14) | curves (monotonic, endpoints, exp slow/fast start, step levels), reversed and clamped ranges, two's complement encoders, latest-wins (three messages → one write), channel and device filters, toggle (presses per frame, CC rising edge), momentary, relative with clamp, pick-up (waits, engages on passing, re-arms after an outside change), colour components merged in one frame, int rounding, menu by index, learn (MIDI with channel and device, OSC, note-off ignored, cancel), monitor cap, OSCQuery routes, OSC over loopback |
| OSCQuery tests (`oscquery::tests`, 5) | JSON shape per kind (TYPE, RANGE, VALUE with live value winning, ACCESS read-only for text), `?VALUE`, 204 for a missing attribute, 404, `HOST_INFO`; fingerprint ignores value edits and sees renames; **HTTP fetch over loopback** (`/`, `?HOST_INFO`, percent-encoded path, 404) with the app side publishing on request; **mDNS**: both services registered and the HTTP service resolved by a second daemon (port checked) in 0.8 s (the test skips with a printed reason where multicast is unavailable) |
| App tests | learn picks toggle/momentary/absolute by kind; saving stores the mapped live value, writes `mappings.toml`, clears the dirty flag, keeps the live value (GPU test) |
| App smoke | `cargo run -p lsr-app -- --smoke 30`: ok (33 frames) |
| Gate | `cargo build --workspace` ok; `cargo test --workspace --no-fail-fast` **550 passed, 0 failed, 9 ignored**; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all -- --check` clean; `cargo deny check` ok; `lasair docs --check` up to date; `lasair check --gpu examples/*.lasair` 44 of 44 ok |
| Flake (not this slice) | One earlier `cargo test --workspace` run failed `lsr-py` `offline_renders_of_python_operators_are_deterministic` (`params= must list lasair.Float(...)`); it failed the same way on the base commit `b8b4e11` in a clean worktree, and passed in the full run above |

Not run: the app with a real MIDI controller, and the app's OSCQuery against a real client (TouchOSC, Chataigne) on the LAN — the server and mDNS were exercised by the tests above, not through the app window; a hand check for Mark.

## Deviations and choices

- **Mapping types live in `lsr-format`** (data plus file form); the runtime in `lsr-io` depends on `lsr-format` and `lsr-core`. `lsr-io` stays independent of `lsr-gpu`: the caller passes a lookup (`mapping::resolve` over the engine's network, registry and live values) and applies `writes()`.
- **OSC settings sit in `mappings.toml`** (`[osc]`), not `project.toml`, so the project file is untouched and the Timeline slice's edits there cannot collide.
- **Targets are in the main network only.**
- **Relative mode:** MIDI two's complement only (the most common encoder setting); binary-offset (64 = no change) and sign-magnitude encoders are not offered yet.
- **Toggle** flips the parameter's current value (so it follows changes made elsewhere), not a remembered state.
- **MIDI inputs** open lazily (first MIDI mapping, learn, or the Mappings window) and then stay open; the runtime opens every input so learn sees all devices. On Windows (WinMM) inputs are exclusive, so a `sig.midi_in` on the same device may then fail. Opening per device pattern instead is possible but learn needs every device.
- **OSCQuery** answers only HTTP GET with `Connection: close`; no WebSocket `LISTEN`/`PATH_CHANGED`. Pulse parameters are advertised as type `I`, write-only.

## Open questions for Mark

1. **Write-back on save** (stores mapped live values as values): keep, or save the stored values untouched (live values are lost on reopen)? I chose write-back because a show reopening with every fader-driven value reset surprised me more than an extra undo step.
2. **MIDI exclusivity on Windows**: accept the lazy "open everything" rule, or share one MIDI input layer between mappings, `sig.midi_in` and the clock (`sig.beat` MIDI clock in), which would also serve the Clock slice?
3. The **default ports** (OSC 9000, OSCQuery HTTP 9001) are common choices that other apps also use; fine?
4. Should mappings be able to target networks other than `main` (a Pack's inside, a second deck network)?
