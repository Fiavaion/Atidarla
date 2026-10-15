//! Media and protocol I/O: video (FFmpeg-LGPL, HAP, hardware decode), audio,
//! cameras, OSC/OSCQuery, MIDI 1/2, Art-Net/sACN, LTC/MTC, serial,
//! HTTP/WebSocket (PRD §5.2).
//!
//! Every endpoint runs on its own I/O thread and exchanges data with the
//! cook path through short, non-blocking hand-offs, so the render thread
//! never waits on a device or the network (CLAUDE.md rule 1).
//!
//! **Status (M1 pre-work):** OSC in and out ([`osc`]); live audio capture ([`audio_in`]); HAP movie playback ([`video`]); MIDI ([`midi`]); DMX over Art-Net and sACN ([`dmx`]). Other protocols land
//! with their operators.

pub mod audio_in;
pub mod dmx;
pub mod image;
pub mod midi;
pub mod osc;
pub mod video;
