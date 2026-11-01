# Spike (e): SD-Turbo 512² StreamDiffusion-style benchmark

**Status:** Apple M5 Pro rows done 2026-10-30 (torch MPS). RTX 4070 / RTX 4090 rows: to run. · **Feeds:** PT-3, PT-4, PT-5, AC11 (confirm or revise PT-3).

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

## Implementation

`ai-host/src/lsr_ai_host/adapters/diffusion.py` (adapter `diffusion.sd`), written from the paper's
equations (arXiv 2312.12491): SDEdit noising, stream batch (one U-Net pass advances a batch whose
elements sit at different steps; deterministic DDIM step between them), R-CFG `self` and `onetime`,
the stochastic similarity filter, TAESD (`AutoencoderTiny`), Prompt Blend. diffusers/transformers
only load the networks. `steps` is the number of U-Net steps; `strength` sets the first timestep
(`t0 = round(1000·strength) − 1`, trailing spacing below it).

Run: `uv run --extra diffusion lsr-ai-host bench --model sd-turbo --size 512 --steps N --frames 600 [...]`
(30 warm-up frames, then 600 timed; moving 8-frame test pattern unless `--static`).

**Licence:** SD-Turbo is under the Stability AI community licence (non-commercial use without a
membership; RESEARCH §4.9). Mark accepted it for benchmarking on 2026-10-30. Weights live in the
model cache (`~/Library/Caches/Lasair/models/sd-turbo`), never in the repo. A commercial-safe
model through the same pipeline is still to do.

## Results

### Apple M5 Pro (48 GB), macOS 26.6.2 — 2026-10-30

torch 2.15.0 (MPS), diffusers 0.42.1, transformers 5.22.1, Python 3.13.18. fp16 U-Net and text
encoder, TAESD. 512², strength 0.5, guidance 1.0 unless stated. "GPU peak" is the MPS driver
allocation; RSS is the bench process.

| mode | steps | guidance / R-CFG | SSF (η) | input | fps | p50 ms | p95 ms | skipped | GPU peak MB | RSS MB |
|---|---|---|---|---|---|---|---|---|---|---|
| direct | 1 | – | off | moving | **11.4** | 86.7 | 107.1 | 0 | 3342 | 597 |
| direct | 2 | – | off | moving | 6.0 | 150.9 | 261.1 | 0 | 3336 | 602 |
| direct | 1 | 1.5, R-CFG self | off | moving | 10.7 | 83.8 | 135.8 | 0 | 3342 | 573 |
| direct | 2 | 1.5, R-CFG self | off | moving | 6.7 | 135.9 | 223.0 | 0 | 3336 | 608 |
| direct | 2 | 1.5, R-CFG onetime | off | moving | 5.5 | 172.0 | 244.0 | 0 | 3330 | 604 |
| direct | 1 | 1.5, full CFG | off | moving | 7.9 | 121.9 | 151.0 | 0 | 3336 | 601 |
| direct | 1 | – | 0.98 | static | 543.1 | 1.7 | 3.4 | 600/600 | 3335 | 582 |
| direct | 2 | – | 0.98 | static | 483.5 | 1.7 | 4.4 | 600/600 | 3328 | 582 |
| protocol (shm, 1 in flight) | 1 | – | off | moving | **10.3** | 87.5 | 150.9 | 0 | 3342 | – |
| protocol (shm, 1 in flight) | 2 | – | off | moving | 7.5 | 121.1 | 175.2 | 0 | 3336 | – |
| protocol (shm, 2 in flight) | 1 | – | off | moving | 9.2 | 184.9 | 352.4 | 0 | 3343 | – |

Stage split for one 1-step frame (synchronised, mean of 20): upload 0.2 ms, TAESD encode 7.6 ms,
U-Net 65 ms, TAESD decode 9.8 ms, download 1.3 ms. `channels_last` makes the MPS U-Net 2.6× slower
(170 ms), so it is off.

**Reading:**
- The U-Net is ~75 % of the frame; the loop around it is lean. The protocol costs ~0.6 ms per frame
  over the host's own time (87.5 vs 86.9 ms at p50), in line with spike (d).
- R-CFG self costs nothing over no guidance (no extra U-Net work) where full CFG costs ~40 %, which is
  the paper's point. Onetime adds one negative element to the batch.
- Two frames in flight do not help on one MPS device: the model's worker serialises them, so latency
  doubles with no gain in throughput. The engine should keep 1 in flight per model on a single-GPU host.
- With the similarity filter on a still input every frame after the first is skipped, so the host
  answers in ~2 ms.
- Run-to-run variance between 2-step runs is large (direct 6.0 fps vs protocol 7.5 fps for the same
  work), probably GPU clocks and thermals. Treat the 2-step numbers as ±20 %.
- **PT-5 is not met on torch MPS** (11 fps at 1 step). It needs Core ML (ANE/GPU, a converted U-Net)
  or a smaller/distilled U-Net. AC4's route (Mac → Windows/NVIDIA LAN host) is the M4 answer for
  real-time diffusion on a Mac.

### RTX 4070 12 GB, Windows — to run

`uv run --extra diffusion lsr-ai-host bench --model sd-turbo --steps 1` (and `--steps 2`,
`--rcfg self`, `--similarity 0.98 --static`, `--via-protocol`). TensorRT engine cache not implemented yet.

### RTX 4090 — to run (PT-4)
