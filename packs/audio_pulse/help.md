# Audio Pulse

Measures the loudness of an audio signal and makes an image brighten and glow with it. The
smoothed level is also an output, so other parameters can follow the same pulse.

## Inputs and outputs

| Socket | Type | What |
|---|---|---|
| `in` | IMG | The image that pulses. |
| `audio` | SIG | Audio-rate channels: Audio In, Audio File In, or anything with samples. |
| `out` | IMG | The pulsing image. |
| `pulse` | SIG | One channel, `level`: Quiet Level in silence, Loud Level at full loudness. |

## Parameters

| Parameter | Default | What it does |
|---|---|---|
| Gain | 4 | Amplifies the audio before measuring. |
| Attack | 0.02 s | How fast the pulse rises. |
| Release | 0.25 s | How fast it falls back. |
| Quiet Level | 0.4 | Brightness and glow in silence. |
| Loud Level | 1.6 | Brightness and glow at full loudness. |

## Tips

- Bind any number parameter to this node's `level` channel to move it with the music: the
  example binds a hue rotation to it.
- With live input, pick the device on an Audio In node and wire it to `audio`.
- Offline renders (`atidarla render`) read audio files exactly; live inputs are idle there.

## Inside

Spectrum (its `level` channel) → Select → Lag (attack, release) → Math (map 0–0.6 to Quiet–Loud)
→ bound to Level brightness and Bloom intensity.

## Versions

- 1.0.0 (2026-10-18): first version.