# Spike (a): Spout and Syphon texture sharing via `wgpu-hal`

**Status:** core mechanism built and tested in CI. Spout protocol layer built, loopback-tested (RTX 5070 Ti and WARP) and **verified against the official Spout SDK (SpoutDX 2.007.018) in both directions** on the RTX 5070 Ti; the remaining gaps are listed under "Still unverified". **Syphon built on the official framework (2026-10-30)** and checked across processes against plain Syphon.framework peers in both directions on an Apple M5 Pro; real Syphon apps not tried yet. · **Feeds:** `lsr-interop`, IMG Share In/Out (M1), RESEARCH §4.8, §5.2, §10 Q3.

## Built so far (2026-10-15)

`crates/lsr-interop` answers questions 1–3 at the mechanism level:

- **Windows:** `dx12::SharedTexture::create` makes a committed D3D12 resource on a `D3D12_HEAP_FLAG_SHARED` heap with `ALLOW_RENDER_TARGET | ALLOW_SIMULTANEOUS_ACCESS`, exports an NT handle (`CreateSharedHandle`, optionally named) and wraps the resource with `wgpu::hal::dx12::Device::texture_from_raw` + `Device::create_texture_from_hal`. `dx12::import` does the reverse with `OpenSharedHandle`, and `open_by_name` does it with `OpenSharedHandleByName`.
- **macOS:** `metal::SharedSurface::create` makes an `IOSurface` (`'BGRA'`/`'RGBA'`, 4 bytes per element), creates an `MTLTexture` on it with `newTextureWithDescriptor:iosurface:plane:`, and wraps that with `wgpu::hal::metal::Device::texture_from_raw`. `metal::lookup(id)` + `metal::import` is the consumer side, as a Syphon client in another process would do it.
- **Tests:** both write a pattern on device A, read it on an independent device B (its own `wgpu::Instance`), then write on B and read on A, with no CPU copy between them, for RGBA8 and BGRA8 (`tests/dx12_share.rs`, `tests/metal_share.rs`). CI runs them on `windows-latest` (WARP) and `macos-latest` (Apple Paravirtual device).
- **Readback fallback:** `readback::read_rgba8`, tested everywhere including Linux/lavapipe.

Still to do on hardware: the Syphon server announcement and its timings (done 2026-10-30, see the Syphon section).

## Spout 2.007 sender and receiver (2026-10-17)

`lsr_interop::spout` (Windows only) implements the Spout protocol in Rust with the `windows` crate, from a reading of the Spout2 SDK source (BSD-2, Lynn Jarvis, commit 6b68a5b). No SDK code is copied into the crate; the SDK is only compiled into the out-of-tree interop peers below.

- **API:** `SpoutSender::new(device, name, width, height, format)` registers the name (`name()` gives the final one after `_1`, `_2` renaming), `send_wgpu(queue, &SharedTexture)` copies a frame in (resizing on a size or format change), `resize(..)`; dropping it removes the name. `SpoutReceiver::list_senders()`, `active_sender()`, `connect(device, Some(name) | None)` (None follows the active sender) and `receive_into_shared(queue, &SharedTexture)`, which returns `NoSender`, `Resize { width, height, format }`, `NewFrame` or `NoNewFrame`.
- **Hand-off:** the D3D11 device is created on the wgpu device's adapter (matched by LUID). Sending opens the `dx12::SharedTexture`'s NT handle with `OpenSharedResource1` and `CopyResource`s it into the Spout texture (D3D11, `MISC_SHARED`, legacy handle), then flushes; receiving opens the sender's legacy handle with `OpenSharedResource` and copies the other way. No CPU readback.
- **wgpu ↔ D3D11 sync: a shared fence**, not `Device::poll`. The fence is created on D3D12 (`D3D12_FENCE_FLAG_SHARED`) and opened with `ID3D11Device5::OpenSharedFence`. Per copy: the wgpu queue signals *n*, the D3D11 context waits for *n*, copies, signals *n+1* and flushes, and the wgpu queue waits for *n+1*. Both waits are on the GPU, so no call blocks the render thread on the GPU, and wgpu cannot overwrite (send) or read (receive) the texture before D3D11 is done with it. `Device::poll` would have blocked the CPU for the whole GPU queue. Requires Windows 10 1703+ (D3D11.4 fences).
- **D3D11 ↔ other Spout processes:** as the SDK: hold `<sender>_SpoutAccessMutex` while copying, `Flush`, count the frame on `<sender>_Count_Semaphore`. Legacy shared textures have no explicit cross-device ordering; hardware drivers sync them implicitly. **WARP does not:** with WARP the loopback tests read zeros unless the sender's copy has finished first, so the test helper waits for it (`finish`) after each send. On the RTX 5070 Ti the tests also passed 15 of 15 runs without that wait.
- **Blocking (rule 1):** every shared-memory access waits up to 67 ms for the map's mutex (sender creation, resize, drop, `list_senders`, `active_sender`, and the info-map read in every `receive_into_shared`). `send_wgpu` and `receive_into_shared` wait up to 67 ms for the access mutex and skip the frame on timeout; holders keep it for microseconds. Creating a sender or receiver creates a D3D11 device. No call waits for the GPU. Per-frame calls allocate nothing in our code after warm-up (rule 2); `send_wgpu` calls `queue.submit([])` to flush pending `write_texture`s.
- **Tests:** `src/spout/shm.rs` unit tests (name-slot layout, `SharedTextureInfo` layout, register/rename/release in the real sender list, no GPU needed) and `tests/spout_loopback.rs` (two wgpu devices on separate instances, one process: pixels equal for BGRA8 and RGBA8, no new frame until the sender sends again, resize and format change mid-stream, sender drop removes the name and the active name, a receiver survives the sender going and coming back, `_1`/`_2` renaming and reuse). Skipped without a GPU unless `LSR_REQUIRE_GPU` is set. Passing on the RTX 5070 Ti and with WARP forced.
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

### Interop with the official Spout SDK (2026-10-17)

Authorised by Mark for this step: build the SDK source and run small test programs against it, nothing downloaded or installed.

**Setup.** Spout2 at commit 6b68a5b ("Version 2.007.018"), cloned to `.scratch\spout2-src` (not vendored). Three C++ peers in [`spout-interop/`](spout-interop/) link the SDK sources of its `SpoutDX_static` CMake target (`SpoutDX.cpp` + `SpoutGL\SpoutCopy`, `SpoutDirectX`, `SpoutFrameCount`, `SpoutSenderNames`, `SpoutSharedMemory`, `SpoutUtils`), unmodified, compiled with MSVC 19.44.35229 x64 (`/O2 /MD /std:c++17`):
- `peer_receive` is an SDK receiver: `ReceiveTexture()`, `IsUpdated()`, `IsFrameNew()`, `GetSenderTexture()`, then its own staging copy and map.
- `peer_send` is an SDK sender: `SendTexture()` of a D3D11 texture filled with `UpdateSubresource`, `HoldFps(60)`.
- `peer_list` prints `GetSenderCount`/`GetSender`/`GetSenderInfo`/`GetActiveSender`.

On our side, the `spout_send` example (`--pattern`, `--timing`, `--resize`, `--rgba`) and `spout_receive --verify` exchange the same **stamped test pattern** (`examples/common/stamped.rs`, mirrored in `peer_common.h`): `readback::test_pattern(w, h, seed)` with the first 16 bytes replaced by frame number, seed and send time (FILETIME ticks from `GetSystemTimePreciseAsFileTime`). Every received frame is read back and compared byte for byte after the stamp.

Machine: Windows 11 (10.0.26200), RTX 5070 Ti (driver 32.0.16.1742), wgpu 30.0.1 DX12, release builds. No `HKCU\Software\Leading Edge\Spout` key exists, so the SDK runs with its defaults: `MaxSenders` 64, `Framecount` off.

**Re-run:** clone `https://github.com/leadedge/Spout2` and check out 6b68a5b (anywhere, e.g. `.scratch\spout2-src`), then `powershell -ExecutionPolicy Bypass -File docs\spikes\spout-interop\run.ps1 -Sdk <Spout2>\SPOUTSDK`. It needs the MSVC x64 build tools. It builds the examples and peers (into `.scratch\spout-peers` by default) and runs every case below, using sender names `lsr-interop-*`.

**Results** (all from one `run.ps1` run; earlier manual runs gave the same outcomes):

| Case | Result |
|---|---|
| Ours → SDK, BGRA8 1920×1080, resized to 960×540 mid-stream | SDK connected (DXGI_FORMAT 87), saw the resize (`IsUpdated`), **353 distinct frames verified, 0 bad**; SDK saw "sender gone" when ours exited |
| Ours → SDK, RGBA8 1280×720 | DXGI_FORMAT 28, **358 frames verified, 0 bad** |
| SDK → ours, BGRA8 1920×1080, resized to 960×540 | our receiver got `Resize` twice, **343 frames verified, 0 bad**; "waiting for a sender" after the SDK sender exited |
| SDK → ours, RGBA8 1280×720 | `Rgba8Unorm`, **364 frames verified, 0 bad** |
| Same name, ours first | SDK sender registered as `lsr-interop-dup_1` and became active; both sides listed `lsr-interop-dup`, `lsr-interop-dup_1` with the same active sender; when the SDK sender exited, `lsr-interop-dup` became active on both sides; empty list after both exited |
| Same name, SDK first | ours registered as `lsr-interop-dup2_1` and became active; an SDK receiver following the active sender connected to ours (60 frames verified); when ours exited, `lsr-interop-dup2` became active on both sides; empty list after both exited |
| Follow active | SDK receiver (no name) found ours; our receiver (`connect(None)`) found the SDK's (timing runs) |

Legacy handles with bit 31 set (`0x8…`, `0xC…`) and without (`0x4…`) were opened in both directions, which covers the 32-bit truncation and sign extension.

**Cross-process timings** (`--timing`: pattern uploaded once, only the 16-byte stamp per frame; 60 fps senders; receivers poll with a 1 ms sleep; ms, p50 / p95):

| Direction, size | Sender call | Receiver call | Stamp → pixels on receiver CPU | Receiver readback | Frames verified |
|---|---|---|---|---|---|
| Ours → SDK, 1080p | `send_wgpu` 0.118 / 0.240 | `ReceiveTexture` 0.065 / 0.108 | 4.66 / 8.33 | copy + map 0.244 | 354, 0 bad, 0 missed |
| SDK → ours, 1080p | `SendTexture` 0.057 / 0.095 | `receive_into_shared` 0.090 / 0.149 | 4.73 / 8.27 | `read_rgba8` 1.165 | 363, 0 bad, 0 missed |
| Ours → SDK, 4K | `send_wgpu` 0.125 / 0.239 | `ReceiveTexture` 0.071 / 0.132 | 9.81 / 17.70 | copy + map 0.739 | 243, 0 bad, 111 missed |
| SDK → ours, 4K | `SendTexture` 0.061 / 0.097 | `receive_into_shared` 0.114 / 0.179 | 11.72 / 19.17 | `read_rgba8` 3.799 | 232, 0 bad, 130 missed |

*Stamp → pixels* runs from the sender's CPU stamp to the pixels being mapped on the receiver's CPU. It includes the stamp upload, both GPU copies, the receiver's polling and the readback, and the gaps between the two 60 Hz loops, so it is an upper bound for the Spout hop and not its GPU cost (the loopback "GPU done" figures in the record are that). The two implementations are symmetric within noise. *Missed* frames at 4K are the receivers' own verification (a 33 MB byte compare plus readback per frame) falling behind 60 fps, not Spout drops. Every frame that was checked matched. Our `send_wgpu` here (0.12 ms) is slower than in the loopback (0.025 ms) because it includes the `queue.submit([])` that flushes that frame's stamp upload; the loopback timing had no pending upload.

**No protocol bug found.** Every case behaved as our implementation expects, and no Rust change was needed. The SDK deviations listed above are invisible to the SDK in these runs.

**The nine facts previously checked only by reading**, now:
1. Shared memory, ANSI names, `<map>_mutex` locks: **verified** (each side opened the other's maps, in both directions).
2. `SpoutSenderNames` layout and sort order: **verified** with the default size (each side listed the other's names in the same order). A `MaxSenders` registry value, or an existing smaller map: **not verified** (the registry was not changed).
3. `_1` renaming, the new sender becoming active, the first remaining sender becoming active on release: **verified** in both orders.
4. `ActiveSenderName`: **verified** (each side read the active name the other wrote).
5. `SharedTextureInfo` handle, width, height and format: **verified**, as the SDK opened ours and we opened its. The description (exe path) and partner-id fields: **not verified**, because no SDK path used here reads them.
6. 32-bit handle with sign extension: **verified** (handles with bit 31 set).
7. Texture description, `OpenSharedResource`, a new handle on resize: **verified** for BGRA8 and RGBA8 in both directions.
8. Access mutex: **verified** in use; both sides took it every frame with no stalls and no bad frames. Frame-count semaphore: only the counting-off case is verified, because the SDK has `Framecount` off here. Its sender does not count, so our receiver treats every frame as new; its receiver ignores our count. With `Framecount` = 1: **not verified** (it needs a registry change).
9. Info map read every frame, sender exit detected when the map goes: **verified** in both directions.

**Still unverified:**
- `Framecount` = 1 and `MaxSenders` registry settings.
- SDK senders other than SpoutDX: SpoutGL senders using GL/DX interop (which set partner-id bits), SpoutDX12, DX9.
- Real applications (Resolume, TouchDesigner, OBS).
- Sender and receiver on different adapters.
- Crashed peers: abandoned mutexes, stale names.
- Keyed-mutex senders.
- Non-ASCII and 255-byte names, and a full 64-sender list.
- WARP or CI against the SDK (WARP lacks the implicit cross-device sync, see above).

## Syphon servers and clients (2026-10-30)

`lsr_interop::syphon` (macOS only) drives the **official Syphon framework**, not a reimplementation: Syphon-Framework (BSD-3-Clause, `third-party/Syphon/License.txt`) at commit f476167 is fetched by `crates/lsr-interop/build.rs` (shallow `git fetch` into `OUT_DIR`, or the checkout in `LSR_SYPHON_SRC` for offline builds), built unmodified with `xcodebuild` (about 5 s; needs Xcode and its Metal compiler for the framework's `default.metallib`) and loaded at run time with `dlopen` from `LSR_SYPHON_FRAMEWORK`, an app bundle's `Contents/Frameworks`, beside the executable, or the build output. Nothing links against it, so binaries start where it is missing and the nodes report why. Nothing is vendored or committed.

- **API:** `SyphonServer::new(device, queue, name)`, `publish(&texture)` (RGBA8 or BGRA8; `SyphonMetalServer publishFrameTexture:onCommandBuffer:...` on a command buffer we commit to the wgpu queue's own `MTLCommandQueue`, so it runs after the frame and Syphon announces the frame once the copy has completed); `SyphonReceiver::connect(device, queue, Some(name) | None)` and `receive_into(&texture)` returning `NoSender`, `Resize`, `NewFrame` or `NoNewFrame` like Spout's; a `SyphonMetalClient` with a frame handler counts announced frames and each new one is one blit into the target on the wgpu queue. Syphon frames are always BGRA8. `list_servers()` merges the directory with this process's own servers.
- **Main run loop:** the directory and the servers' answers to "announce yourselves" travel as distributed notifications, which macOS delivers **only on the main thread's run loop** (checked: a directory created on another thread with its own run loop heard nothing). The app's event loop runs it. A headless process (CLI, tests) does not: it neither sees other processes' servers nor answers late clients, so this process's servers are also kept in a local list, which is what the loopback tests and `lasair check` use. `syphon::run_main_loop` exists for tools and tests that own their main thread.
- **Syphon bug worked around:** a client's "remove me" message still queued when its server stops is delivered to the server's freed message receiver (use-after-free in `SyphonServerConnectionManager`, crash in `objc_retain` on the `info.v002.syphon.messaging` queue). Our parallel loopback tests hit it 1 run in 10. Dropping a `SyphonServer` now waits up to 100 ms for `hasClients` to turn false (the last client message handled); 0 crashes in 20 runs after. Worth reporting upstream.
- **First frame:** a client learns its server's surface before the server's first copy has run, so a local server's first frame is copied only once its command buffer has completed (a completion handler counter); another process's server is taken at its word, as Syphon itself does.
- **Tests:** `tests/syphon_loopback.rs` (two wgpu devices on separate instances: exact bytes both ways, RGBA8 source arrives as BGRA8, no new frame until the next publish, resize mid-stream, server gone and back, newest server, name matching, bad names) and `tests/syphon_directory.rs` (`harness = false`, runs the main run loop: the directory lists the server and drops it when it stops). Operator tests `lsr-ops-img/tests/share.rs` run on macOS too.

### Cross-process check against plain Syphon apps (2026-10-30)

`docs/spikes/syphon-interop/run.sh` builds `peer_send.m` and `peer_receive.m` (minimal Syphon apps using only `Syphon.framework` and Metal: `SyphonMetalServer` publishing a CPU-filled texture; `SyphonServerDirectory` + `SyphonMetalClient` with a frame handler, blit to a shared texture, `getBytes`) and runs them against the `syphon_send` / `syphon_receive` examples with the same stamped pattern as the Spout check, compared byte for byte after the stamp. Apple M5 Pro, macOS 26.6, Xcode 27.1, wgpu 30.0.1 Metal, release builds, 60 fps senders:

| Case | Result |
|---|---|
| Ours → peer, BGRA8 1920×1080 resized to 960×540, peer started 1 s after | peer found our server through the directory (announce request), saw both sizes, **409 distinct frames verified, 0 bad**; saw "server gone" when ours exited |
| Peer → ours, BGRA8 1920×1080 resized to 960×540, ours started 1 s after | our receiver listed `peer_send`'s server, followed both sizes, **373 frames verified, 0 bad**, "waiting for a server" after it exited |
| Ours → ours, receiver waiting first, 1280×720 | found the server when it started (its announcement), **240 of 240 frames verified, 0 bad** |
| The app (`lasair-app examples/texture-sharing.lasair --smoke 600`) → peer | peer connected to "Lasair Example" from `lasair-app`, 640×360 BGRA8, 166 frames in 5 s (noise, so not pattern-checked); the app's own Share In showed the loopback |

Timings from that run (ms, mean / p50 / p95): `publish` 0.11 / 0.08 / 0.22 at 1080p; `receive_into` 0.17 / 0.07 / 0.19; stamp → pixels on the receiver's CPU, including its blocking readback and polling, 4.1 / 2.4 / 14.4. Skipped frame numbers (9 and 17) are the receivers' blocking readbacks falling behind, not Syphon drops.

**Still unverified (Syphon):** real apps (Resolume, VDMX, MadMapper, Syphon Simple Client/Server, OBS with a Syphon plugin); a 4K run and the GPU-only timings the Spout record has; Intel Macs; the CI `macos-latest` runner (the build fetches and builds the framework there; not yet pushed); headless (CLI) servers answering clients that start later, which needs the main run loop.

## Questions to answer
1. Can we export a `wgpu::Texture` created on the DX12 backend as a shared handle that Spout 2.007.x receivers accept (format `B8G8R8A8_UNORM` / `R8G8B8A8_UNORM`, keyed mutex or none)?
2. Can we import a Spout sender's shared handle as a `wgpu::Texture` without a copy (`wgpu-hal` DX12 `texture_from_raw`, then `Device::create_texture_from_hal`)?
3. On macOS, can we wrap an `IOSurface` as an `MTLTexture` and then as a `wgpu::Texture` (`wgpu-hal` Metal `texture_from_raw`), and publish our own IOSurface as a Syphon server?
4. What does each path cost per 1080p and 4K frame, compared with the readback fallback?

## Procedure
1. Add `lsr-interop` examples: `spout_send`, `spout_receive` (done), `syphon_send`, `syphon_receive`. Each animates a test pattern rendered by wgpu.
2. Windows: check against the Spout demo receiver/sender (Spout2 SDK, BSD-2); done with SpoutDX peers built from the SDK source, see above. macOS: check against Syphon Simple Client/Server.
3. Measure with GPU timestamps: time from the sender's frame submit to the receiver's first sample. Record copy and no-copy variants and the readback fallback.

## Record
| Path | OS | Res | Zero-copy? | Latency ms | Notes |
|---|---|---|---|---|---|
| Spout send | Windows | 1080p / 4K | no CPU copy; one GPU copy | CPU call 0.025 / 0.027 (p50), 0.060 / 0.069 (p95) | `send_wgpu`, RTX 5070 Ti |
| Spout receive | Windows | 1080p / 4K | no CPU copy; one GPU copy | CPU call 0.032 / 0.033 (p50), 0.066 / 0.076 (p95) | `receive_into_shared` |
| Spout send → receive, GPU done | Windows | 1080p / 4K | as above | 0.218 / 0.275 (p50), 0.245 / 0.308 (p95) | loopback, both GPU copies finished |
| Spout ours ↔ SDK, cross-process | Windows | 1080p / 4K | as above | stamp → receiver CPU 4.7 / 9.8–11.7 (p50) | upper bound incl. polling and readback; see the interop section |
| Syphon send | macOS | 1080p | no CPU copy; one GPU blit by Syphon | CPU call 0.08 (p50), 0.22 (p95) | `SyphonServer::publish`, Apple M5 Pro, cross-process run; 4K not measured |
| Syphon receive | macOS | 1080p | no CPU copy; one GPU blit | CPU call 0.07 (p50), 0.19 (p95) | `SyphonReceiver::receive_into`; 4K not measured |
| Readback fallback | Windows | 1080p / 4K | n/a | 1.186 / 4.339 (p50), 1.419 / 4.463 (p95) | `read_rgba8` of the received BGRA8 texture, one side only |

Spout method (2026-10-17): `cargo test -p lsr-interop --release --test spout_loopback -- --ignored --nocapture` (`loopback_timing`), RTX 5070 Ti, Windows 11, wgpu 30.0.1 DX12, BGRA8, three runs of 300 frames per size. Figures are the middle run; across the three runs the CPU-call p50s were within 0.02 ms of each other, GPU-done p50s within 0.01 ms and readback p50s within 0.1 ms. One process, two wgpu devices on separate instances; the same 1080p or 4K source is sent every frame, so each frame is a new one for the receiver. *CPU call* is wall-clock time inside `send_wgpu` / `receive_into_shared`, which do not wait for the GPU. *GPU done* is from the start of `send_wgpu` until the receiving device's `poll(Wait)` returns after its queue has passed the fence the receiving D3D11 device signals after its copy; it includes both D3D11 copies and fence round trips, but no cross-process scheduling. *Readback* is `read_rgba8` (GPU copy to a mapped buffer, map, de-pad) of the received texture, 30 runs: the fallback would pay this plus an upload on the other side.

Licences: Spout is BSD-2. Syphon-Framework is **BSD-3-Clause** (checked in its `License.txt`, 2026-10-30; copied to `third-party/Syphon/License.txt`; binary distributions must carry it).
