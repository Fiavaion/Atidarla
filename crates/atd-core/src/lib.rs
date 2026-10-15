//! Atidarla core: graph store, typed sockets, parameter system, pull cook
//! scheduler, time model, undo/redo and the operator registry (PRD §5.2).
//!
//! **Status (M0):** the operator registry and the capability-flag mechanism
//! (PRD A19) are implemented. The graph store, parameter system and cook
//! scheduler land in M1.

pub mod capability;
pub mod platform;
pub mod registry;

pub use capability::{CapabilityFlag, CapabilityRequirement, Fallback};
pub use platform::{Arch, GpuVendor, Os, PlatformInfo};
pub use registry::{Availability, OpFamily, OperatorSchema, Registry};

/// The Atidarla version string, shared by the app, CLI and bench output.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
