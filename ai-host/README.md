# atd-ai-host

The AI Host for Atidarla (PRD §5.1, A6). It runs model work (torch, ONNX
Runtime, later TensorRT and Core ML) **out of process**, so a crash or a slow
model can never stall the engine's render thread. It speaks AI Host Protocol
v1 (`docs/ai-host-protocol.md`). No model weights are ever bundled: models live
in the per-user cache (`~/Library/Caches/Atidarla/models`,
`%LOCALAPPDATA%\Atidarla\models`, `$XDG_CACHE_HOME/atidarla/models`;
`ATD_MODELS_DIR` overrides).

## Install

```sh
uv sync                                   # protocol + built-in models only (standard library)
uv sync --extra diffusion --extra vision  # + SD-Turbo (torch, diffusers) and ONNX vision models
uv run pytest                             # tests needing extras or weights skip without them
```

| Extra | Adapter | Runs |
|---|---|---|
| (none) | `builtin` | `builtin.invert`, `builtin.echo` (reference models for tests) |
| `diffusion` | `diffusion.sd` | SD-Turbo-class img2img: stream batch, R-CFG, similarity filter, TAESD, Prompt Blend |
| `vision` | `onnx` | any `.onnx` vision model: depth, segment, pose, flow, background, upscale, generic |
| – | `video.v2v` | interface only; backends (Decart) arrive later |

## Run

**Local (spawned by the engine).** The engine runs one host and opens a session
per AI node, each with its own shared-memory ring:

```sh
atd-ai-host --listen 127.0.0.1:0 --token <random> --parent-pid <engine pid>
# stdout: atd-ai-host listening on 127.0.0.1:<port>
```

`--connect HOST:PORT` (dial the engine, one session) still works. `--parent-pid`
makes the host exit within about a second of the engine dying.

**LAN (e.g. a Windows/NVIDIA PC serving a Mac).** Anything but loopback needs a
token; the engine sends it in `session.open`:

```sh
atd-ai-host --listen 0.0.0.0:7420 --token <secret>   # or ATD_AI_HOST_TOKEN=<secret>
```

**Cloud (Runpod).** `Dockerfile` builds a CUDA image that listens on 7420 and
reads the token from `ATD_AI_HOST_TOKEN`; mount the model cache at `/models`.

**Development.** `atd-ai-host --dev` listens on 127.0.0.1 and logs verbosely.

## Benchmark

```sh
uv run --extra diffusion atd-ai-host bench --model sd-turbo --size 512 --steps 1 --frames 600
uv run --extra diffusion atd-ai-host bench --steps 2 --rcfg self            # R-CFG (self-negative)
uv run --extra diffusion atd-ai-host bench --similarity 0.98 --static       # similarity filter on a still input
uv run --extra diffusion atd-ai-host bench --via-protocol [--inflight 2]    # through a host and the shm ring
```

`--model` takes a folder or a name in the model cache. It prints one JSON line:
steady-state fps, p50/p95/max frame ms and peak memory, after a warm-up.
Results live in `docs/spikes/e-sd-turbo-benchmark.md`.
