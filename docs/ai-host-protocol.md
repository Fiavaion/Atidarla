# AI Host Protocol v1

The contract between the Atidarla engine and an AI Host: the local Python host
(`ai-host/`), the same host listening on a LAN machine, or a Runpod pod running
the host's container (FR-A1, PRD §5.1). It replaces the M0 prototype (`0.1`,
`docs/spikes/d-ai-host-protocol.md`); version strings must match exactly.

`PROTOCOL_VERSION = "1.0"`

## Connection

- **Local:** the engine's supervisor listens on `127.0.0.1:0` and spawns the
  host with `--connect 127.0.0.1:<port>`. Frames go through a shared-memory
  ring (`transport.kind = "shm"`).
- **LAN / cloud:** the host runs `atd-ai-host --listen <addr>:<port> --token <t>`
  and the engine connects to it. Frames travel on the socket
  (`transport.kind = "inline"`). A host listening on anything but loopback
  refuses to start without `--token` (or `ATD_AI_HOST_TOKEN`).
- One TCP stream per session. `TCP_NODELAY` on both ends.

## Framing

Messages are JSON-RPC 2.0 objects, one per line (`\n`). A message may carry a
binary payload: its JSON has `"payload": <bytes>` at the top level, and exactly
that many raw bytes follow the newline. Only frame messages on the `inline`
transport use payloads.

Requests may be pipelined: the engine can have several requests in flight and
the host answers each by `id`, possibly out of order (a `host.health` is
answered while a frame is being processed). Frames for one model are processed
in submission order.

The host may send **notifications** (no `id`): `host.log {level, message}`
and `model.progress {model_id, stage, fraction}`.

## Methods

| Method | Params | Result |
|---|---|---|
| `session.open` | `SessionOpen` | `SessionInfo` |
| `host.capabilities` | – | `Capabilities` |
| `host.health` | – | `Health` |
| `model.load` | `ModelLoad` | `ModelInfo` |
| `model.unload` | `{model_id}` | `null` |
| `frame.submit` | `FrameSubmit` | `FrameResult` |
| `frame.cancel` | `{seq}` | `null` |
| `session.close` | – | `null` |

```text
SessionOpen  { protocol_version: "1.0", client: str,
               transport: { kind: "shm", ring_path: str } | { kind: "inline", codecs: [str] },
               token: str | null }
SessionInfo  { session_id, protocol_version, host_name, host_version,
               transport: "shm" | "inline", codec: "rgba8" | "jpeg" | null }

Capabilities { vendor: "nvidia"|"amd"|"intel"|"apple"|"cpu",
               devices: [{ name: str, vendor: str, vram_mb: int|null }],
               vram_mb: int|null,                  // largest device
               runtimes: [str],                    // "torch-cuda","torch-mps","tensorrt","onnxruntime-coreml","onnxruntime-cpu","cpu-reference",...
               transports: ["shm","inline"],
               adapters: [str],                    // adapter kinds this host can run, see below
               models: [str] }                     // model ids loadable without a path (built-ins)

Health       { uptime_s, frames_processed, loaded_models: [str], queue_depth: int, gpu_mem_mb: int|null }

ModelLoad    { model_id: str, adapter: str, path: str|null, options: object }
ModelInfo    { model_id, load_ms, adapter, device: str }

FrameSubmit  { seq: u64, timestamp_us: u64, model_id: str,
               width: u32, height: u32,
               input_slot: u32|null, output_slot: u32|null,   // shm only
               aux: { <name>: slot },                          // shm: extra inputs (e.g. "control")
               params: object,                                  // per-adapter, see below
               payload?: bytes }                                // inline: input pixels follow (codec from session)
FrameResult  { seq, width, height, output_slot: u32|null, host_ms: f64,
               skipped: bool,           // the similarity filter reused the previous output
               payload?: bytes }        // inline: output pixels follow
```

Pixels are RGBA8, unpremultiplied, rows top to bottom, tightly packed
(`width * 4` bytes a row). On `inline` with codec `jpeg` the payload is a JPEG
(alpha is 255 on return). Aux inputs on `inline` are not supported in v1.

### Error codes

| Code | Name | Meaning |
|---|---|---|
| -32001 | `VERSION_MISMATCH` | `protocol_version` differs |
| -32002 | `UNKNOWN_MODEL` | model not loaded / unknown built-in |
| -32003 | `BAD_SLOT` | slot out of range or holds another seq |
| -32004 | `STALE_FRAME` | frame cancelled or superseded |
| -32005 | `UNAUTHORIZED` | missing or wrong token |
| -32006 | `BUSY` | queue full; resubmit later |
| -32007 | `MODEL_LOAD_FAILED` | adapter missing, weights unreadable, out of memory |
| -32008 | `UNSUPPORTED` | adapter or transport not available on this host |

## Adapters and their `params`

| Adapter | Model ids | `FrameSubmit.params` |
|---|---|---|
| `builtin` | `builtin.invert`, `builtin.echo` | – (`echo` returns its input; used by tests) |
| `diffusion.sd` | e.g. `sd-turbo` (path = the model folder) | `prompts: [{text, weight}]`, `negative: str`, `seed: int`, `strength: f32` (0–1), `steps: int` (1–4), `guidance: f32`, `rcfg: "none"\|"self"\|"onetime"`, `delta: f32`, `similarity: f32` (0 = off, else η), `lora: [{path, weight}]` |
| `onnx` | any (path = the `.onnx` file) | `task: "generic"\|"depth"\|"segment"\|"pose"\|"flow"\|"background"\|"upscale"`, `input: {name, layout, mean, std, size}` (generic only) |
| `video.v2v` | per model | `prompts`, `seed` |

`prompts` is the Prompt Blend (FR-A4): the host encodes each distinct text once,
caches the embedding and uses the weighted sum (weights normalised to sum 1).

## Timing and liveness

- Every call has a read timeout of 5 s (60 s for `model.load`).
- The supervisor sends `host.health` every second; two missed answers, a
  closed socket or a dead process mean the host has failed. A local host is
  restarted (PT-9: producing frames again within 5 s); the node holds its last
  good frame meanwhile.
