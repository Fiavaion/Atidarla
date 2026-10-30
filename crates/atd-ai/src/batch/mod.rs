//! Batch Graph (FR-B1–B3, D8): wedges of work items over parameters, run
//! across AI Hosts, cached by content hash and kept in the Stage media bin.
//!
//! A wedge (`atd_format::batch`) makes work items: parameter overrides
//! applied to a copy of the network, each cooked offline for one target
//! node. This module is the engine-free part:
//!
//! - [`key`]: the cache key of an item, a SHA-256 over the target's
//!   upstream subgraph after overrides (node types, every parameter's
//!   resolved value, wiring, bindings, pinned model checksums), the frame
//!   and the size. Where an item runs is not part of it.
//! - [`place`]: which hosts a scheduler names and which of them can run
//!   the item's AI nodes (round-robin by capability, FR-B2).
//! - [`run`]: the runner: skips items whose image is already in the bin,
//!   hands the rest to one worker per host slot (each with its own
//!   [`Cooker`], e.g. an offline engine whose AI nodes are pinned to that
//!   host), writes each image to `media/bin/<key>.png` and its entry to
//!   `media/bin/index.toml` as it lands.
//!
//! Nothing here runs on a render thread: workers are threads of their own
//! (CLAUDE.md rule 1). See `docs/batch.md`.

pub mod key;
pub mod place;
pub mod run;

pub use key::{KeyInput, cache_key, is_placement};
pub use place::{Picked, pick_hosts};
pub use run::{Cooker, Event, Image, Item, Override, Report, RunOptions, run};
