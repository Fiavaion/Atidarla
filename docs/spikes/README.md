# M0 spikes

Each spike is a short report with numbers (PRD §9, M0). A spike that has not been run says so and gives the exact procedure, so whoever has the hardware can run it.

| Spike | Report | Status |
|---|---|---|
| (a) Spout + Syphon texture sharing via `wgpu-hal` | [a-texture-sharing.md](a-texture-sharing.md) | Not run (needs Windows + macOS) |
| (b) CUDA external memory / IPC → wgpu on Windows | [b-cuda-interop.md](b-cuda-interop.md) | Not run (needs Windows + NVIDIA) |
| (c) macOS signing + notarisation, embedded Python, signed AI Host helper | [c-macos-signing.md](c-macos-signing.md) | Not run (needs a Mac + Developer ID) |
| (d) AI Host Protocol prototype: shared memory + GPU handle | [d-ai-host-protocol.md](d-ai-host-protocol.md) | Shared-memory path done; GPU handle pending |
| (e) SD-Turbo 512² StreamDiffusion-style benchmark | [e-sd-turbo-benchmark.md](e-sd-turbo-benchmark.md) | Not run (needs RTX 4070 + M-series) |
| MSH library licences (M0 acceptance) | [msh-licences.md](msh-licences.md) | Done; Geogram needs sign-off |
