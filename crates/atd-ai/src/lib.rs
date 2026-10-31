//! AI Host protocol, supervisor, compute router, provider adapters, model
//! manager and Output Governor (PRD §5.1, §5.2 `atd-ai`, §7.5).
//!
//! **Status (M0):** spike (d), a prototype of AI Host Protocol v1 (FR-A1):
//! JSON-RPC control over a local stream socket, and frames passed through a
//! shared-memory ring. See `docs/spikes/d-ai-host-protocol.md`. The final
//! protocol, supervisor and governor land in M4.
//!
//! Nothing here may be called from the render thread: AI is asynchronous
//! and reaches the frame loop only through the Output Governor
//! (CLAUDE.md rule 1).

pub mod batch;
pub mod client;
pub mod cloud;
pub mod comfy;
pub mod egress;
pub mod hostenv;
pub mod hosts;
pub mod link;
pub mod llm;
pub mod models;
pub mod protocol;
pub mod refhost;
pub mod ring;
pub mod router;
pub mod speech;
pub mod supervisor;

pub use client::{ClientError, FramePath, HostClient, HostCommand, Pending};
pub use protocol::PROTOCOL_VERSION;
pub use ring::{FrameRing, RingLayout};
