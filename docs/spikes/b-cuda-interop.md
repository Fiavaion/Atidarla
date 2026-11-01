# Spike (b): CUDA external memory / IPC import to wgpu on Windows

**Status:** not run. Needs Windows + an NVIDIA GPU (lab baseline RTX 4070). · **Feeds:** spike (d) GPU-handle transport, FR-A1, PT-3/PT-4.

## Questions to answer
1. Can a texture or buffer allocated by wgpu (DX12 backend) be exported as an NT shared handle and imported in CUDA with `cudaImportExternalMemory` (type `D3D12Resource`)? And the reverse direction?
2. Can the host process (torch) read and write that memory through `cudaExternalMemoryGetMappedBuffer`, wrapped as a torch tensor via DLPack, with no host copy?
3. How do the two sides synchronise: a D3D12 fence shared as a CUDA external semaphore, or wgpu `Queue::on_submitted_work_done` plus a control message?
4. Cross-process: does the NT handle survive `DuplicateHandle` into the AI Host process, and is the cost per frame flat?

## Procedure
1. `lsr-interop` example `cuda_export`: create a 512² RGBA8 storage texture through `wgpu-hal` DX12 with `D3D12_HEAP_FLAG_SHARED` and export the handle.
2. In `ai-host`, a test script that imports it via `cuda-python`, wraps it with DLPack, runs `x = 1 - x`, and signals the fence.
3. Compare with spike (d)'s shared-memory path at 512² and 1024².

## Record
| Direction | Res | Import once ms | Per-frame overhead ms | Sync method | Notes |
|---|---|---|---|---|---|
| wgpu → CUDA | 512² | | | | |
| CUDA → wgpu | 512² | | | | |
| round trip | 1024² | | | | |
