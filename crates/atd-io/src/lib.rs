//! Media and protocol I/O: video (FFmpeg-LGPL, HAP, hardware decode), audio,
//! cameras, OSC/OSCQuery, MIDI 1/2, Art-Net/sACN, LTC/MTC, serial,
//! HTTP/WebSocket (PRD §5.2).
//!
//! Every endpoint runs on its own I/O thread and exchanges data with the
//! cook path through short, non-blocking hand-offs, so the render thread
//! never waits on a device or the network (CLAUDE.md rule 1).
//!
//! **Status (M1 pre-work):** OSC in and out ([`osc`]). Other protocols land
//! with their operators.

pub mod osc;
