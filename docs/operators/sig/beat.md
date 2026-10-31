# Beat (`sig.beat`)

SIG · version 1 · cook budget 20 µs

A tempo clock: `beat` count, `beat_phase` 0–1 within the beat, `bar` count and `bar_phase`, the tempo `bpm`, and one-frame pulses `downbeat` (a bar starts) and `beat_pulse` (a beat starts). Runs from its own tempo (with tap, nudge and resync), follows a MIDI clock, or follows beats in an audio onset input.

Cooks every frame (time-dependent).

## Inputs

| Name | Type | Notes |
|---|---|---|
| `in` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `bpm` | BPM | float | `120.0` | 0 – 999 | Tempo in beats per minute; changes keep the phase continuous. Tapping replaces it until this value is changed. |
| `beats_per_bar` | Beats per Bar | int | `4` | 1 – 64 | Beats in one bar. |
| `reset` | Reset | toggle | `false` |  | While on, holds the clock at beat zero. |
| `source` | Source | menu | `"internal"` | `internal`, `midi`, `audio` | `internal` runs at BPM. `midi` follows a MIDI clock (24 pulses per quarter note, Start, Stop, Continue, Song Position). `audio` follows beats in the input, for example the `onset` channel of an Onset operator. |
| `tap` | Tap | pulse | `false` |  | Tap to set the tempo: the mean interval of the last 2–8 taps; a gap over 2 seconds starts again. |
| `nudge` | Nudge | float | `0.0` | -1 – 1 | While away from 0, speeds the internal clock up (+) or slows it (−) by this fraction of Nudge Amount, to pull a drifting beat back in line. |
| `nudge_amount` | Nudge Amount | float | `6.0` | 0 – 50 | Largest tempo change from Nudge, in percent. |
| `resync` | Resync | pulse | `false` |  | Makes the frame it fires on beat 0 of a bar (bar 0): the next downbeat is here. |
| `device` | Device | string | `""` |  | Source `midi`: part of the input port's name; empty for the first port; `virtual` for no device. |
| `send_clock` | Send MIDI Clock | toggle | `false` |  | Sends MIDI clock (24 pulses per quarter note) with Start, Continue and Stop to Out Device, from a timing thread so pulses keep their spacing at any frame rate. Not with source `midi`, and silent in offline renders. |
| `out_device` | Out Device | string | `""` |  | Part of the output port's name for the MIDI clock; empty for the first port; `virtual` for no device. |
| `min_bpm` | Min BPM | float | `70.0` | 20 – 255 | Source `audio`: lowest tempo to report; intervals are folded into this range. |
| `max_bpm` | Max BPM | float | `180.0` | 20 – 255 | Source `audio`: highest tempo to report. |
| `smoothing` | Smoothing | float | `0.7` | 0 – 0.99 | Source `audio`: how slowly the tempo and phase follow new onsets (0 = at once). |

## Platforms

All platforms.

## Examples

- [Beat clock](../../../examples/beat-clock.atidarla/) — `clock`
- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `clock`
- [Soak show](../../../examples/soak-show.atidarla/) — `clock`
- [Stage deck](../../../examples/stage-deck.atidarla/) — `clock`
