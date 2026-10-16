# MIDI In (`sig.midi_in`)

SIG · version 1 · cook budget 20 µs

Channels from a MIDI input: controllers (`ch1_cc7`), notes (velocity while held, 0 after release), pitch bend, pressure and program, added as they arrive. The port runs on its own thread.

Cooks every frame (time-dependent).

## Inputs

None.

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `device` | Device | string | `""` |  | Part of the input port's name; empty for the first port; `virtual` for no device. |
| `channel` | Channel | int | `0` | 0 – 16 | MIDI channel to listen to (1–16), or 0 for all; channel names drop the `chN_` prefix when one is chosen. |
| `normalise` | Normalise | toggle | `true` |  | Scale values to 0–1 (pitch bend to −1…1) instead of raw MIDI values. |

## Platforms

All platforms.

## Examples

- [Live control](../../../examples/live-control.atidarla/) — `midi_in1`
