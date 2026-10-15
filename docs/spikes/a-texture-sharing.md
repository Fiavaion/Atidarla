# Spike (a): Spout and Syphon texture sharing via `wgpu-hal`

**Status:** core mechanism built and tested in CI; Spout/Syphon protocol wire-up and timings need a Windows PC and a Mac. · **Feeds:** `atd-interop`, IMG Share In/Out (M1), RESEARCH §4.8, §5.2, §10 Q3.

## Built so far (2026-10-15)

`crates/atd-interop` answers questions 1–3 at the mechanism level:

- **Windows:** `dx12::SharedTexture::create` makes a committed D3D12 resource on a `D3D12_HEAP_FLAG_SHARED` heap with `ALLOW_RENDER_TARGET | ALLOW_SIMULTANEOUS_ACCESS`, exports an NT handle (`CreateSharedHandle`, optionally named) and wraps the resource with `wgpu::hal::dx12::Device::texture_from_raw` + `Device::create_texture_from_hal`. `dx12::import` does the reverse with `OpenSharedHandle`, and `open_by_name` does it with `OpenSharedHandleByName`.
- **macOS:** `metal::SharedSurface::create` makes an `IOSurface` (`'BGRA'`/`'RGBA'`, 4 bytes per element), creates an `MTLTexture` on it with `newTextureWithDescriptor:iosurface:plane:`, and wraps that with `wgpu::hal::metal::Device::texture_from_raw`. `metal::lookup(id)` + `metal::import` is the consumer side, as a Syphon client in another process would do it.
- **Tests:** both write a pattern on device A, read it on an independent device B (its own `wgpu::Instance`), then write on B and read on A, with no CPU copy between them, for RGBA8 and BGRA8 (`tests/dx12_share.rs`, `tests/metal_share.rs`). CI runs them on `windows-latest` (WARP) and `macos-latest` (Apple Paravirtual device).
- **Readback fallback:** `readback::read_rgba8`, tested everywhere including Linux/lavapipe.

Still to do on hardware: the Spout sender/receiver handshake (Spout's shared-memory sender list, D3D11 receivers via `OpenSharedResource1`), the Syphon server announcement, cross-process timing, and the results table below.

## Questions to answer
1. Can we export a `wgpu::Texture` created on the DX12 backend as a shared handle that Spout 2.007.x receivers accept (format `B8G8R8A8_UNORM` / `R8G8B8A8_UNORM`, keyed mutex or none)?
2. Can we import a Spout sender's shared handle as a `wgpu::Texture` without a copy (`wgpu-hal` DX12 `texture_from_raw`, then `Device::create_texture_from_hal`)?
3. On macOS, can we wrap an `IOSurface` as an `MTLTexture` and then as a `wgpu::Texture` (`wgpu-hal` Metal `texture_from_raw`), and publish our own IOSurface as a Syphon server?
4. What does each path cost per 1080p and 4K frame, compared with the readback fallback?

## Procedure
1. Add `atd-interop` examples: `spout_send`, `spout_receive`, `syphon_send`, `syphon_receive`. Each animates a test pattern rendered by wgpu.
2. Windows: check against the Spout demo receiver/sender (Spout2 SDK, BSD-2). macOS: check against Syphon Simple Client/Server.
3. Measure with GPU timestamps: time from the sender's frame submit to the receiver's first sample. Record copy and no-copy variants and the readback fallback.

## Record
| Path | OS | Res | Zero-copy? | Latency ms | Notes |
|---|---|---|---|---|---|
| Spout send | Windows | 1080p / 4K | | | |
| Spout receive | Windows | 1080p / 4K | | | |
| Syphon send | macOS | 1080p / 4K | | | |
| Syphon receive | macOS | 1080p / 4K | | | |
| Readback fallback | both | 1080p | n/a | | |

Licences: Spout is BSD-2. Syphon is BSD-style [unverified, RESEARCH §5.2]; confirm its licence before writing bindings.
