//! Timeline (PRD §7.4 FR-T1–T3, decision A54): keyframe tracks for any
//! number parameter, markers, cues with conditions, a transport, and
//! timecode chase.
//!
//! - The document is `timeline.toml` in the project
//!   (`lsr_format::timeline::TimelineDoc`).
//! - [`Timeline`] binds it to a network and, each frame, evaluates tracks
//!   at the playhead into live values and fires cues the playhead crossed
//!   (allocation-free after binding).
//! - [`Transport`] moves the playhead: play, pause, stop, loop, scrub, on
//!   the local clock or chasing MTC/LTC ([`chase`]).
//! - [`Player`] drives an engine with both: `Engine::set_live` for tracks
//!   and held cue values, edits for cue `set` and `launch` actions (live
//!   only; offline renders hold every action as a live value).
//!
//! User reference: `docs/timeline.md`.

pub mod chase;
pub mod curve;
mod eval;
mod player;
pub mod transport;

pub use chase::{Chase, ChaseConfig, ChaseState, LtcInput, MtcInput};
pub use eval::{NoSignals, Op, OpKind, Signals, Slot, Timeline};
pub use player::{EngineSignals, Player};
pub use transport::{ClockSource, Motion, PlayState, Transport};
