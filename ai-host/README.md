# atd-ai-host

The local AI Host for Atidarla (PRD §5.1, A6). It runs model work (torch,
TensorRT, ONNX Runtime, Core ML, ComfyUI connector) **out of process**, so a
crash or a slow model can never stall the engine's render thread.

**Status (M0):** a prototype of AI Host Protocol v1 (spike d,
`docs/spikes/d-ai-host-protocol.md`). It speaks JSON-RPC over a local socket,
exchanges RGBA8 frames through a shared-memory ring, and ships a single
built-in reference model, `builtin.invert`. No model weights are ever bundled.

```sh
uv sync
uv run atd-ai-host --dev            # listen on 127.0.0.1 and log verbosely
uv run atd-ai-host --connect HOST:PORT   # how the engine's supervisor starts it
uv run pytest
```
