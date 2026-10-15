//! Per-OS GPU texture sharing via `wgpu-hal`: Spout + CUDA external memory (Windows), Syphon/IOSurface (macOS), readback fallback; Linux best-effort (PRD §5.2, spikes a and b).
//!
//! **Status:** M0 stub. Implementation lands in M1 (see `PRD.md` §9).
