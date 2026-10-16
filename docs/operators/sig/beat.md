# Beat (`sig.beat`)

SIG · version 1 · cook budget 20 µs

A tempo clock: `beat` count, `beat_phase` 0–1 within the beat, `bar` count and `bar_phase`.

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
| `bpm` | BPM | float | `120.0` | 0 – 999 | Tempo in beats per minute; changes keep the phase continuous. |
| `beats_per_bar` | Beats per Bar | int | `4` | 1 – 64 | Beats in one bar. |
| `reset` | Reset | toggle | `false` |  | While on, holds the clock at beat zero. |

## Platforms

All platforms.

## Examples

- [Beat switcher](../../../examples/beat-switcher.atidarla/) — `clock`
