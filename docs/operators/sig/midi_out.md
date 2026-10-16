# MIDI Out (`sig.midi_out`)

SIG · version 1 · cook budget 20 µs

Sends each input channel (0–1) as a MIDI control change, numbered from First CC, whenever its value changes; passes the input through.

Sink: cooked every frame even when nothing downstream is viewed, because it has external side effects.

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
| `device` | Device | string | `""` |  | Part of the output port's name; empty for the first port; `virtual` for no device. |
| `channel` | Channel | int | `1` | 1 – 16 | MIDI channel to send on. |
| `first_cc` | First CC | int | `1` | 0 – 127 | Controller number for the first input channel; the rest follow. |

## Platforms

All platforms.

## Examples

- [Live control](../../../examples/live-control.atidarla/) — `midi_out1`
