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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// The local address a sending UDP socket binds to. A loopback target is
/// reached from a loopback socket, which Windows Firewall does not prompt for;
/// anything else (the LAN, a broadcast) needs the wildcard address. Binding the
/// wildcard unconditionally made every new test executable ask for firewall
/// access, and each answer left a rule behind.
pub(crate) fn source_addr(target: SocketAddr) -> SocketAddr {
    let ip: IpAddr = match (target.ip().is_loopback(), target.is_ipv4()) {
        (true, true) => Ipv4Addr::LOCALHOST.into(),
        (true, false) => Ipv6Addr::LOCALHOST.into(),
        (false, true) => Ipv4Addr::UNSPECIFIED.into(),
        (false, false) => Ipv6Addr::UNSPECIFIED.into(),
    };
    SocketAddr::new(ip, 0)
}

pub mod audio_in;
pub mod audio_out;
pub mod dmx;
pub mod image;
pub mod input;
pub mod midi;
pub mod osc;
pub mod timecode;
pub mod video;
