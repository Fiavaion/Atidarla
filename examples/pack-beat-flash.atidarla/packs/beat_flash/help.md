# Beat Flash

A tempo clock that flashes an image brighter on every beat. The flash envelope is an output, so
other parameters can jump on the beat too.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image that flashes. |
| `out` | IMG | The flashing image. |
| `flash` | SIG | One channel, `trigger`: 1 between flashes, Intensity at the peak. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| BPM | 120 | Tempo. |
| Decay | 0.3 s | How long a flash takes to fade. |
| Intensity | 2.5 | Peak brightness (1 means no flash). |
| Restart | off | Holds the clock at the start of a bar while on; switch it off on the downbeat to sync by hand. |

## Tips

- Bind BPM to an external tempo (OSC In, MIDI In) to follow a DJ.
- Bind another parameter to this node's `trigger` channel (for example a Transform's rotate) to
  move things on the beat.

## Inside

Beat → Select (`beat_phase`) → Trigger (fires as the phase wraps) → Math (maps 0–1 to 1–Intensity)
→ bound to Level brightness.

## Versions

- 1.0.0 (2026-10-18): first version.