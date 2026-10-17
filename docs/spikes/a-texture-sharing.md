# Spike (a): Spout and Syphon texture sharing via `wgpu-hal`

**Status:** core mechanism built and tested in CI. Spout protocol layer built and loopback-tested on Windows (RTX 5070 Ti and WARP), but **interoperation with the official Spout SDK is not yet verified** (needs a decision from Mark, see below). Syphon wire-up and timings still need a Mac. · **Feeds:** `atd-interop`, IMG Share In/Out (M1), RESEARCH §4.8, §5.2, §10 Q3.

## Built so far (2026-10-15)

`crates/atd-interop` answers questions 1–3 at the mechanism level:

- **Windows:** `dx12::SharedTexture::create` makes a committed D3D12 resource on a `D3D12_HEAP_FLAG_SHARED` heap with `ALLOW_RENDER_TARGET | ALLOW_SIMULTANEOUS_ACCESS`, exports an NT handle (`CreateSharedHandle`, optionally named) and wraps the resource with `wgpu::hal::dx12::Device::texture_from_raw` + `Device::create_texture_from_hal`. `dx12::import` does the reverse with `OpenSharedHandle`, and `open_by_name` does it with `OpenSharedHandleByName`.
- **macOS:** `metal::SharedSurface::create` makes an `IOSurface` (`'BGRA'`/`'RGBA'`, 4 bytes per element), creates an `MTLTexture` on it with `newTextureWithDescriptor:iosurface:plane:`, and wraps that with `wgpu::hal::metal::Device::texture_from_raw`. `metal::lookup(id)` + `metal::import` is the consumer side, as a Syphon client in another process would do it.
- **Tests:** both write a pattern on device A, read it on an independent device B (its own `wgpu::Instance`), then write on B and read on A, with no CPU copy between them, for RGBA8 and BGRA8 (`tests/dx12_share.rs`, `tests/metal_share.rs`). CI runs them on `windows-latest` (WARP) and `macos-latest` (Apple Paravirtual device).
- **Readback fallback:** `readback::read_rgba8`, tested everywhere including Linux/lavapipe.

Still to do on hardware: the Syphon server announcement and its timings.

## Spout 2.007 sender and receiver (2026-10-17)

`atd_interop::spout` (Windows only) implements the Spout protocol in Rust with the `windows` crate, from a reading of the Spout2 SDK source (BSD-2, Lynn Jarvis, commit 6b68a5b). No SDK code is compiled, run or copied.

- **API:** `SpoutSender::new(device, name, width, height, format)` registers the name (`name()` gives the final one after `_1`, `_2` renaming), `send_wgpu(queue, &SharedTexture)` copies a frame in (resizing on a size or format change), `resize(..)`; dropping it removes the name. `SpoutReceiver::list_senders()`, `active_sender()`, `connect(device, Some(name) | None)` (None follows the active sender) and `receive_into_shared(queue, &SharedTexture)`, which returns `NoSender`, `Resize { width, height, format }`, `NewFrame` or `NoNewFrame`.
- **Hand-off:** the D3D11 device is created on the wgpu device's adapter (matched by LUID). Sending opens the `dx12::SharedTexture`'s NT handle with `OpenSharedResource1` and `CopyResource`s it into the Spout texture (D3D11, `MISC_SHARED`, legacy handle), then flushes; receiving opens the sender's legacy handle with `OpenSharedResource` and copies the other way. No CPU readback.
- **wgpu ↔ D3D11 sync: a shared fence**, not `Device::poll`. The fence is created on D3D12 (`D3D12_FENCE_FLAG_SHARED`) and opened with `ID3D11Device5::OpenSharedFence`. Per copy: the wgpu queue signals *n*, the D3D11 context waits for *n*, copies, signals *n+1* and flushes, and the wgpu queue waits for *n+1*. Both waits are on the GPU, so no call blocks the render thread on the GPU, and wgpu cannot overwrite (send) or read (receive) the texture before D3D11 is done with it. `Device::poll` would have blocked the CPU for the whole GPU queue. Requires Windows 10 1703+ (D3D11.4 fences).
- **D3D11 ↔ other Spout processes:** as the SDK: hold `<sender>_SpoutAccessMutex` while copying, `Flush`, count the frame on `<sender>_Count_Semaphore`. Legacy shared textures have no explicit cross-device ordering; hardware drivers sync them implicitly. **WARP does not:** with WARP the loopback tests read zeros unless the sender's copy has finished first, so the test helper waits for it (`finish`) after each send. On the RTX 5070 Ti the tests also passed 15 of 15 runs without that wait.
- **Blocking (rule 1):** every shared-memory access waits up to 67 ms for the map's mutex (sender creation, resize, drop, `list_senders`, `active_sender`, and the info-map read in every `receive_into_shared`). `send_wgpu` and `receive_into_shared` wait up to 67 ms for the access mutex and skip the frame on timeout; holders keep it for microseconds. Creating a sender or receiver creates a D3D11 device. No call waits for the GPU. Per-frame calls allocate nothing in our code after warm-up (rule 2); `send_wgpu` calls `queue.submit([])` to flush pending `write_texture`s.
- **Tests:** `src/spout/shm.rs` unit tests (name-slot layout, `SharedTextureInfo` layout, register/rename/release in the real sender list, no GPU needed) and `tests/spout_loopback.rs` (two wgpu devices on separate instances, one process: pixels equal for BGRA8 and RGBA8, no new frame until the sender sends again, resize and format change mid-stream, sender drop removes the name and the active name, a receiver survives the sender going and coming back, `_1`/`_2` renaming and reuse). Skipped without a GPU unless `ATD_REQUIRE_GPU` is set. Passing on the RTX 5070 Ti and with WARP forced.
- **Two processes:** `examples/spout_send.rs` (animated wgpu pattern, 60 fps) and `examples/spout_receive.rs` (size + FNV-1a checksum) were run against each other at 1080p and 640×360: the receiver got ~59 new frames per second with changing checksums, waited for a sender that started late, and survived it quitting. This shows our two sides agree, not that we match the SDK.

### Deviations from the SDK (deliberate)

- The SDK's lock timeouts are **67 ms** (`SpoutSharedMemory::Lock`, `spoutFrameCount::CheckAccess`), not `SPOUT_WAIT_TIMEOUT` (100, defined in `SpoutSenderNames.h` but unused). We use 67 ms.
- An abandoned mutex (its owner died holding it) counts as acquired. The SDK treats `WAIT_ABANDONED` as a failure and then never releases the mutex.
- The info map is created and written with the sender list locked, so a receiver never sees a name without its map. The SDK registers the name, unlocks, then creates the map.
- When the released sender was the last one and was active, we clear `ActiveSenderName`; the SDK leaves the stale name (its readers then fail to open its map, the same outcome).
- Our sender always counts frames and our receiver always reads the count; the SDK does both only when `HKCU\Software\Leading Edge\Spout\Framecount` is 1. A receiver that finds no count treats every frame as new, so this is harmless either way.
- The list's capacity is `min(MaxSenders, mapped size / 256)`, so a smaller map made by an older Spout cannot be overrun. A full list is an error rather than the SDK's silent skip.
- Names are 1–255 bytes, no NUL or backslash, passed as raw bytes to the ANSI APIs. Non-ASCII names round-trip between UTF-8 apps but show as mojibake in ANSI-codepage apps.
- Not implemented (opt-in or legacy in the SDK): keyed-mutex and NT-handle sender textures, the `<sender>_Sync_Event` frame-sync event, CPU/memory-share senders (share handle 0, reported as no sender), DX9 senders' formats beyond BGRA/RGBA, receiving formats other than B8G8R8A8/R8G8B8A8 (UNORM or sRGB, an error), switching to the sender's adapter, cleaning stale names left by crashed senders, `SpoutPanel` selection.

### Not yet verified: interoperation with the official Spout SDK

Interop with real Spout apps (the SDK demo sender/receiver, Resolume, TouchDesigner, OBS) is **not verified**. Building or running SDK code or downloaded Spout binaries was not authorised for this step; **Mark needs to decide** whether to allow that (e.g. the SDK's prebuilt demo receiver, or building `SpoutDX` from the BSD-2 source in `.scratch`) or test against an installed Spout app by hand. Every protocol fact below was checked **only by reading the source**, and our loopback only shows our sender and receiver agree with each other:

1. Shared memory via `CreateFileMappingA(INVALID_HANDLE_VALUE, …)`, ANSI names, no `Global\` prefix; each map guarded by `CreateMutexA("<map>_mutex")`; existing maps keep their size (`SpoutSharedMemory.cpp`).
2. `SpoutSenderNames`: `MaxSenders` × 256 bytes, `MaxSenders` from `HKCU\Software\Leading Edge\Spout` (default 64); sorted (`std::set`, byte order) NUL-terminated slots ended by an empty slot (`SpoutSenderNames.cpp`: `readSenderSetFromBuffer`, `writeBufferFromSenderSet`, `CreateSenderSet`).
3. Taken names get `_1`, `_2`, … (`RegisterSenderName` with `bNewname`); the new sender becomes active; on release the first remaining sender becomes active if the released one was active or one is left (`ReleaseSenderName`).
4. `ActiveSenderName`: a 256-byte map holding the name, kept open by the sender (`setActiveSenderName`).
5. Per-sender map named exactly the sender name, 280 bytes, `SharedTextureInfo` { u32 shareHandle, width, height, format (DXGI), usage (0), u8 description[256] (exe path), u32 partnerId (0 for D3D11 senders) } (`SpoutSenderNames.h`, `SetSenderInfo`).
6. Share handle stored truncated to 32 bits (`PtrToUint`) and sign-extended by receivers (`LongToHandle`).
7. Sender texture: D3D11, `BIND_RENDER_TARGET | BIND_SHADER_RESOURCE`, `USAGE_DEFAULT`, 1 mip, 1 sample, `MISC_SHARED`, handle from `IDXGIResource::GetSharedHandle`; receivers `OpenSharedResource`; a resize makes a new texture and handle (`SpoutDirectX.cpp`: `CreateSharedDX11Texture`, `OpenDX11shareHandle`; `SpoutDX.cpp`: `CheckSender`, `ReceiveSenderData`).
8. Frame sync: `<sender>_SpoutAccessMutex` held around `CopyResource` + `Flush`; `<sender>_Count_Semaphore` (initial 1, max `LONG_MAX`), sender `Wait(0)` + `Release(2)`, receiver `Wait(0)` + `Release(1, &previous)` (`SpoutFrameCount.cpp`: `CreateAccessMutex`, `CheckAccess`, `EnableFrameCount`, `SetNewFrame`, `GetNewFrame`).
9. Receivers open the info map on every frame and treat its disappearance as the sender closing (`getSharedInfo`, `FindSender`).

## Questions to answer
1. Can we export a `wgpu::Texture` created on the DX12 backend as a shared handle that Spout 2.007.x receivers accept (format `B8G8R8A8_UNORM` / `R8G8B8A8_UNORM`, keyed mutex or none)?
2. Can we import a Spout sender's shared handle as a `wgpu::Texture` without a copy (`wgpu-hal` DX12 `texture_from_raw`, then `Device::create_texture_from_hal`)?
3. On macOS, can we wrap an `IOSurface` as an `MTLTexture` and then as a `wgpu::Texture` (`wgpu-hal` Metal `texture_from_raw`), and publish our own IOSurface as a Syphon server?
4. What does each path cost per 1080p and 4K frame, compared with the readback fallback?

## Procedure
1. Add `atd-interop` examples: `spout_send`, `spout_receive` (done), `syphon_send`, `syphon_receive`. Each animates a test pattern rendered by wgpu.
2. Windows: check against the Spout demo receiver/sender (Spout2 SDK, BSD-2). macOS: check against Syphon Simple Client/Server.
3. Measure with GPU timestamps: time from the sender's frame submit to the receiver's first sample. Record copy and no-copy variants and the readback fallback.

## Record
| Path | OS | Res | Zero-copy? | Latency ms | Notes |
|---|---|---|---|---|---|
| Spout send | Windows | 1080p / 4K | no CPU copy; one GPU copy | CPU call 0.025 / 0.027 (p50), 0.060 / 0.069 (p95) | `send_wgpu`, RTX 5070 Ti |
| Spout receive | Windows | 1080p / 4K | no CPU copy; one GPU copy | CPU call 0.032 / 0.033 (p50), 0.066 / 0.076 (p95) | `receive_into_shared` |
| Spout send → receive, GPU done | Windows | 1080p / 4K | as above | 0.218 / 0.275 (p50), 0.245 / 0.308 (p95) | loopback, both GPU copies finished |
| Syphon send | macOS | 1080p / 4K | | | |
| Syphon receive | macOS | 1080p / 4K | | | |
| Readback fallback | Windows | 1080p / 4K | n/a | 1.186 / 4.339 (p50), 1.419 / 4.463 (p95) | `read_rgba8` of the received BGRA8 texture, one side only |

Spout method (2026-10-17): `cargo test -p atd-interop --release --test spout_loopback -- --ignored --nocapture` (`loopback_timing`), RTX 5070 Ti, Windows 11, wgpu 30.0.1 DX12, BGRA8, three runs of 300 frames per size. Figures are the middle run; across the three runs the CPU-call p50s were within 0.02 ms of each other, GPU-done p50s within 0.01 ms and readback p50s within 0.1 ms. One process, two wgpu devices on separate instances; the same 1080p or 4K source is sent every frame, so each frame is a new one for the receiver. *CPU call* is wall-clock time inside `send_wgpu` / `receive_into_shared`, which do not wait for the GPU. *GPU done* is from the start of `send_wgpu` until the receiving device's `poll(Wait)` returns after its queue has passed the fence the receiving D3D11 device signals after its copy; it includes both D3D11 copies and fence round trips, but no cross-process scheduling. *Readback* is `read_rgba8` (GPU copy to a mapped buffer, map, de-pad) of the received texture, 30 runs: the fallback would pay this plus an upload on the other side.

Licences: Spout is BSD-2. Syphon is BSD-style [unverified, RESEARCH §5.2]; confirm its licence before writing bindings.
