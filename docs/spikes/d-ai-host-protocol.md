# Spike (d): AI Host Protocol prototype

**Date:** 2026-10-15 · **Status:** shared-memory path done; GPU-handle path pending (needs Windows + NVIDIA)
**Code:** `crates/lsr-ai` (`protocol.rs`, `ring.rs`, `client.rs`, `tests/host_roundtrip.rs`, `examples/spike_d.rs`) and `ai-host/` (Python host).
**Requirement:** FR-A1 (M0 prototype, M4 final), PT-9 groundwork.

## Design

- **Control channel:** JSON-RPC 2.0, one message per line, over a local stream socket. The engine's supervisor listens on `127.0.0.1:0` and spawns the host with `--connect <addr>`, so no port can race. This is TCP loopback for the prototype because it is identical on every OS; a Unix socket or Windows named pipe can replace it in M4 behind the same framing.
- **Methods:** `session.open` (version negotiation, ring path), `host.capabilities` (vendor, VRAM, runtimes, transports, models), `host.health` (heartbeat), `model.load`, `model.unload`, `frame.submit`, `frame.cancel`, `session.close`. Errors are typed codes: `VERSION_MISMATCH`, `UNKNOWN_MODEL`, `BAD_SLOT`, `STALE_FRAME`.
- **Frame transport (all OSes):** a memory-mapped ring file of fixed RGBA8 slots, on `/dev/shm` on Linux and the temp directory elsewhere. Pixels never cross the socket. `frame.submit` names an input slot and an output slot, and the host stamps the output slot with the frame's sequence ID. The client rejects a result whose stamp doesn't match, which catches stale or torn frames without any cross-process locks.
- **Sequence IDs and timestamps** on every frame give the Output Governor (M4) what it needs to drop late frames.
- **Failure behaviour:** every call has a 5 s read timeout. A killed host surfaces as an error within that window, never a hang (`killed_host_fails_fast_instead_of_hanging`). The supervisor's restart loop (FR-A2, ≤5 s, PT-9) is M4 work on top of this.
- **Reference model:** `builtin.invert` (RGB inverted, alpha kept). It is deterministic, so tests can check output byte for byte without model weights.

## Results (shared-memory path)

Machine: cloud container, 4 vCPU Intel Xeon @ 2.10 GHz, Linux, Python 3.13.16, release build. The calls are serial, with one frame in flight.
`cargo run -p lsr-ai --release --example spike_d 300`

| size | frames | spawn+open ms | p50 ms | p95 ms | max ms | host p50 ms | fps (serial) |
|---|---|---|---|---|---|---|---|
| 256² | 300 | 63 | 1.04 | 1.32 | 2.50 | 0.89 | 923 |
| 512² | 300 | 53 | 3.99 | 4.36 | 6.64 | 3.80 | 247 |
| 1024² | 300 | 47 | 16.31 | 20.34 | 26.67 | 16.01 | 59 |

**Reading:** round-trip time minus the host's own processing time is the transport overhead: about 0.15–0.3 ms at p50, at every size. The control message is a ~150-byte JSON line and the pixels stay in shared memory. Host time is dominated by the pure-Python reference model copying the frame (`bytes.translate`). A real model works in place on a torch/NumPy view, so the copy-out is one `memcpy`. At 512², the size PT-3 uses, the transport costs well under 1 ms against a 33 ms budget at 30 fps. **The shared-memory path is not a bottleneck for PT-3.**

## Not yet done

1. **GPU-handle transport (Windows + NVIDIA):** export a wgpu texture as a D3D12 shared handle, import it in the host with CUDA external memory (`cudaImportExternalMemory`), and run a torch op on it with no readback. Advertise it as `transports: ["shm", "cuda-ipc"]`. Depends on spike (b). Measure the same table with `--transport cuda-ipc`.
2. **Pipelining:** the numbers above are serial. M4 will keep 2–3 frames in flight across ring slots, which the slot design already allows.
3. **Run on Windows and macOS:** the integration tests run in CI on both (`cargo test -p lsr-ai`). Re-run `spike_d` on the RTX 4070 PC and an M-series Mac and add rows here.
