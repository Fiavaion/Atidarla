# Spike (a): Spout and Syphon texture sharing via `wgpu-hal`

**Status:** not run. Needs a Windows PC (DX12) and a Mac (Metal). · **Feeds:** `atd-interop`, IMG Share In/Out (M1), RESEARCH §4.8, §5.2, §10 Q3.

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
