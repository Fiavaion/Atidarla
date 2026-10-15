# Spike (e): SD-Turbo 512² StreamDiffusion-style benchmark

**Status:** not run. Needs an RTX 4070 Windows PC and an Apple M-series Mac (M2 Pro or later for PT-5). · **Feeds:** PT-3, PT-4, PT-5, AC11 (confirm or revise PT-3).

## Rules
- Clean-room only (CLAUDE.md rule 5): implement StreamDiffusion techniques from the paper and Apache-2.0 sources (cumulo-autumn/StreamDiffusion, daydreamlive). No closed or Patreon-gated code.
- No weights in the repo (rule 4). SD-Turbo is **non-commercial** (Stability community licence, RESEARCH §4.9). Using it for a benchmark is fine. Record the licence in the report, and run a commercial-safe model through the same pipeline for comparison.

## Configurations
| Machine | Runtime | Techniques |
|---|---|---|
| RTX 4070 12 GB, Windows | torch + TensorRT engine cache | stream batch (denoise steps = 1–2), residual CFG, stochastic similarity filter, tiny VAE (TAESD), fp16 |
| RTX 4070, Windows | torch eager (no TensorRT) | same, without engine cache |
| Apple M-series | Core ML / MLX / MPS | as many techniques as port cleanly |

## Measure
img2img at 512², 1-step and 2-step: steady-state fps over 600 frames after warm-up, p95 frame time, VRAM, and end-to-end latency through the spike (d) shared-memory host. Also run it through the AI Host, not just a standalone script, to measure what the engine will actually see.

## Decide
PT-3 says ≥30 fps (floor 24) on an RTX 4070. Confirm it or propose a revised number in PRD §8 with this data.
