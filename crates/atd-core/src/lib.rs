//! Atidarla core: graph store, typed sockets, parameter system, pull cook
//! scheduler, time model, undo/redo and the operator registry (PRD §5.2).
//!
//! **Status:** the operator registry and capability flags (M0, PRD A19), and
//! M1 pre-work: the graph store with invertible edits ([`graph`]).

pub mod capability;
pub mod cook;
pub mod expr;
pub mod fragment;
pub mod geo;
pub mod graph;
pub mod history;
pub mod pack;
pub mod platform;
pub mod registry;
pub mod wgsl;

pub use capability::{CapabilityFlag, CapabilityRequirement, Fallback};
pub use platform::{Arch, GpuVendor, Os, PlatformInfo};
pub use registry::{Availability, OpFamily, OperatorSchema, Registry};

/// The Atidarla version string, shared by the app, CLI and bench output.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
