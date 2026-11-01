# M3 clock slice (FR-S4)

2026-10-30. Beat clock with tap, nudge, resync, MIDI clock in and out, audio follow, beat-synced
LFO and Timer. Ableton Link is out of scope (A14 is still Proposed). Architecture: A54 in
[`../M3.md`](../M3.md). The Stage reads the clock's channels for quantised launch.

## Built

| What | Where |
|---|---|
| `sig.beat`: `source` (internal, midi, audio), `tap`, `nudge` + `nudge_amount`, `resync`, `device` (MIDI in), `min_bpm`/`max_bpm`/`smoothing` (audio), `send_clock` + `out_device` (MIDI out). Optional SIG input `in` (the onset, for `audio`). Channels exactly `beat`, `beat_phase`, `bar`, `bar_phase`, `bpm`, `downbeat`, `beat_pulse` | `crates/lsr-ops-sig/src/clock.rs` |
| MIDI messages Clock, Start, Continue, Stop, Song Position; `parse_at`; `now_micros`; `MidiOut` short messages (`send_raw`), `OutHandle`, recorded timestamps for detached ports | `crates/lsr-io/src/midi.rs` |
| `ClockFollower`: tempo from a 23-interval window of pulse arrival times, position from counted pulses, extrapolated to the frame's time, Start/Continue/Stop/Song Position | `crates/lsr-io/src/midi_clock.rs` |
| `ClockOut`: a timing thread sending pulses every 1/24 beat, Start, Song Position + Continue, Stop | `crates/lsr-io/src/midi_clock_out.rs` |
| `sig.lfo`, `sig.timer`: optional SIG input `clock`, param `sync` (off, beats) | `crates/lsr-ops-sig/src/ops.rs` |
| Example with tap, synced LFOs and a synced Timer | `examples/beat-clock.lasair` |

### Semantics worth knowing

- **Tap**: rising edge of `tap` (a pulse, or a mapped button). Tempo is the mean interval of the last 2 to 8 taps; a gap over 2 s starts a new count. The tapped tempo replaces `bpm` until `bpm` is edited. Tap sets tempo only, not phase; `resync` sets phase.
- **Nudge**: internal clock only. `nudge` (-1 to 1) times `nudge_amount` (percent, default 6) scales the tempo while it is held.
- **Resync**: the frame it fires on is bar 0, beat 0, with `downbeat` and `beat_pulse` set. For `midi`, the master's current position becomes the display's zero until the next Start, Continue or Song Position.
- **Pulses**: `beat_pulse` is 1 on the frame the beat count changes (or on a jump: resync, Start, Continue, Song Position), `downbeat` on the frame a bar starts. The first cook is both. A jump into the middle of a bar is not a downbeat.
- **MIDI in**: 24 ppqn. A port that only sends pulses (no Start) runs from 0; Stop holds; Start rewinds to 0 and the first pulse after it is beat 0; Continue resumes from the Song Position. Arrival time is stamped in the driver callback on `lsr_io::midi::now_micros()` (driver timestamps have a different origin per platform), the tempo is the window average so callback jitter averages out, and between pulses the position is extrapolated at that tempo, at most one pulse ahead, never stepping backwards.
- **Audio**: onsets (the `onset` channel, else channel 0, above 0.5) vote all pair intervals, folded by 1 to 8 beats into `min_bpm`..`max_bpm`, into a 0.5 BPM histogram; the peak, refined by a duration-weighted mean, is smoothed into the tempo. The first three onsets snap the phase to the nearest beat, later ones pull it by `1 - smoothing`; four off-beat hits in a row (more than 0.3 beat off) re-acquire.
- **MIDI out is on `sig.beat`, not `sig.midi_out`**: the beat clock owns tempo and transport (the start, stop, rewind and tempo changes that must be turned into Start, Stop and pulses); `sig.midi_out` is a per-channel CC sender that passes its input through and has neither. A separate node would need the bpm and a start/stop wired through channels with a frame of latency. The pulses come from `ClockOut`'s thread, so spacing is independent of the frame rate.
- **Sync (LFO, Timer)**: position is the clock's `beat + beat_phase` (f64). LFO: `frequency` in cycles per beat. Timer: `length` in beats, the count is the clock's beats since an origin of 0, so cycles line up with the bar; `reset` restarts the count at the current beat; pause holds the count; a clock that rewinds (resync) takes the count back to 0. The Timer's `seconds` channel reads in beats when synced. Without a clock input (or without `beat`/`beat_phase`) the operator reports an error.
- **Offline (`CookMode::Offline`)**: the internal clock integrates the fixed step; taps use cook time; the `midi` source opens a detached port (no messages, the clock sits at beat 0); `send_clock` sends nothing. Classes in `render.rs` and `docs/render.md` do not change (Beat stays Stateful, LFO Pure, Timer Stateful); a note was added to `docs/render.md`.

## Evidence

Run on an Apple M5 Pro, macOS, 2026-10-30. No MIDI hardware: the follower is driven by synthetic
message streams (no device needed), the sender is read from a detached port.

| Claim | Test | Result |
|---|---|---|
| MIDI clock tempo accuracy | `cargo test -p lsr-io --lib tempo_accuracy -- --nocapture` | 120 BPM: 119.9999 clean (0.0001 %), 120.147 with ±500 µs arrival jitter (0.12 %). 174 BPM: 173.9999 clean (0.0001 %), 174.310 with jitter (0.18 %). Tempo change 120 to 150 within a beat (error under 0.1 %) |
| Phase lock after Start | `start_locks_the_first_pulse_to_beat_zero` | Pulse k after Start reads exactly k/24 beats (error under 1e-9). Between pulses at 120 BPM with ±400 µs jitter, worst error against the ideal grid 0.00084 beat |
| Stop, Continue, Song Position | `stop_holds_continue_resumes_and_song_position_jumps`; op level `midi_clock_source_follows_tempo_and_locks_to_start`, `midi_song_position_and_resync` | Stop holds the position; Song Position 8 sixteenths is beat 2; Continue resumes there; Start rewinds with `downbeat` and `beat_pulse` set |
| A stalled master does not run away | `a_stalled_master_does_not_run_away` | Position stops at most one pulse past the last |
| Clock out pulse spacing | `cargo test -p lsr-io --lib midi_clock_out -- --nocapture` | 120 BPM over 1 s: 47 to 50 pulses, mean spacing 20.83 to 20.91 ms (nominal 20.833), typical interval 20.83 ms to within 0.05 ms, with occasional single wake-ups several ms late when the machine is busy |
| Pulses are not once per frame | `clock_out_pulses_come_from_the_timing_thread_not_the_frame` | Two cooks only, 500 ms later 22 to 26 pulses; a tempo change to 240 BPM reaches the thread; resync sends Start; reset sends Stop and silences the pulses; release starts again from 0 |
| Audio follow | `audio_source_finds_tempo_and_locks_onto_the_beat` | Kick at 128 BPM, 60 fps: tempo 127.98 after 14 s; worst onset-to-beat offset after 8 s 0.016 beat |
| Old projects load unchanged | `a_network_saved_before_the_beat_clock_grew_still_loads` (lsr-format); `an_old_projects_parameters_cook_as_before` | A network with only `bpm`/`beats_per_bar`, saved before this slice, loads, validates, keeps its values, defaults `source` to internal and saves back byte for byte; its first four channels cook as before |
| Synced LFO and Timer are phase-locked | `synced_lfo_is_phase_locked_to_the_clock_beat`, `synced_timer_counts_clock_beats_in_step_with_the_bar`, `synced_lfo_needs_a_clock_and_unsynced_ignores_one` | One cycle per beat equals the clock's `beat_phase` at any beat count, wall time and frame length; 0.25 cycles per beat repeats every 4 beats; a resync takes the wave with it |
| Offline determinism | `cargo test -p lsr-ops-sig --test clock_offline` | 1200 frames of Beat (taps, nudge, resync, reset at scripted frames) plus synced LFO and Timer: bit-identical on two runs; frame i matches the closed form; `midi` source and `send_clock` are idle offline |
| Real engine, two renders | `lasair render examples/beat-clock.lasair --range 0-120` twice | The two frame sets hash identically (120 PNGs) |
| No allocation after warm-up, no panics | `cargo test -p lsr-ops-sig --test alloc`, `--test fuzz` | Pass with the new parameters and inputs |

## Open questions

1. **Live latency of the MIDI follower.** Arrival time is taken in the driver callback; the cook extrapolates. Real devices (a USB clock source over CoreMIDI, WinMM on Windows) need a run on hardware to measure phase against a known master; the synthetic jitter used here is ±400 to ±500 µs. Windows (WinMM/WinRT) was not run.
2. **Tap sets tempo only.** Many DJ and VJ tools also align phase to the tap. Left to `resync`; say if tap should snap the beat too.
3. **Audio source with clock out.** Clock out follows the tempo only (it free-runs from `bpm`); a phase pull from audio onsets does not move the sent grid.
4. **Link (A14).** Out of scope until A14 is ratified. `source` would gain `link`; Link provides tempo, beat and Start/Stop sync in the same shape.
5. **Quantised launch precision.** The Stage reads `beat`, `beat_phase`, `bar`, `downbeat` per frame, so a launch lands on the next frame boundary after the beat, up to one frame late (16.7 ms at 60 fps). Sub-frame launch would need the layer to read the fractional crossing from `beat_phase`; not done.
6. **MIDI clock out device selection** is a plain port-name substring (as `sig.midi_out`); the app's MIDI settings page does not exist yet.
