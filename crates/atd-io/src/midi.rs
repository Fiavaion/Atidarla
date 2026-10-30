//! MIDI 1.0 input and output (`midir`: WinMM/WinRT, CoreMIDI, ALSA).
//!
//! [`MidiIn`] opens its port on a thread. The driver callback parses
//! messages into a short queue, which the cook drains with `try_lock`
//! (rule 1). [`MidiOut`] hands messages to its own thread through a
//! channel, so sending never blocks the cook. Either can be created
//! detached (no device) to inject or capture messages, for tests and
//! virtual routing.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Microseconds on a process-wide monotonic clock. Real-time messages
/// carry their arrival time on it, so the beat clock can extrapolate to
/// the cook's "now" (driver timestamps have a different origin on every
/// platform).
pub fn now_micros() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// A channel-voice message; channels are 1–16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
    },
    Control {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// −8192…8191, centre 0.
    PitchBend {
        channel: u8,
        value: i16,
    },
    ChannelPressure {
        channel: u8,
        value: u8,
    },
    Program {
        channel: u8,
        program: u8,
    },
    /// MIDI Time Code quarter frame (`F1`): piece 0–7 and its 4-bit value.
    QuarterFrame {
        piece: u8,
        value: u8,
    },
    /// MIDI Time Code full frame (universal real-time SysEx): rate code
    /// 0 = 24, 1 = 25, 2 = 29.97 drop-frame, 3 = 30 fps.
    TimecodeFull {
        hours: u8,
        minutes: u8,
        seconds: u8,
        frames: u8,
        rate: u8,
    },
    /// Timing clock (`F8`), 24 per quarter note. `micros` is the arrival
    /// time on [`now_micros`]'s clock.
    Clock {
        micros: u64,
    },
    /// Start (`FA`): play from the beginning at the next clock.
    Start {
        micros: u64,
    },
    /// Continue (`FB`): play from the song position at the next clock.
    Continue {
        micros: u64,
    },
    /// Stop (`FC`).
    Stop {
        micros: u64,
    },
    /// Song Position Pointer (`F2`) in MIDI beats (sixteenth notes).
    SongPosition {
        sixteenths: u16,
    },
}

/// Parses one complete MIDI message (as delivered by `midir`). Other
/// system and unknown messages give `None`. Note-on with velocity 0 is a
/// note-off. Real-time messages get `micros` 0; see [`parse_at`].
pub fn parse(bytes: &[u8]) -> Option<Message> {
    parse_at(bytes, 0)
}

/// [`parse`], stamping real-time messages with their arrival time.
pub fn parse_at(bytes: &[u8], micros: u64) -> Option<Message> {
    let status = *bytes.first()?;
    match status {
        0xF8 => return Some(Message::Clock { micros }),
        0xFA => return Some(Message::Start { micros }),
        0xFB => return Some(Message::Continue { micros }),
        0xFC => return Some(Message::Stop { micros }),
        0xF2 => {
            let (lsb, msb) = (*bytes.get(1)?, *bytes.get(2)?);
            return (lsb < 0x80 && msb < 0x80).then_some(Message::SongPosition {
                sixteenths: (u16::from(msb) << 7) | u16::from(lsb),
            });
        }
        _ => {}
    }
    if status == 0xF1 {
        let d = *bytes.get(1)?;
        return (d < 0x80).then_some(Message::QuarterFrame {
            piece: d >> 4,
            value: d & 0x0F,
        });
    }
    // F0 7F <device> 01 01 hh mm ss ff F7
    if let [0xF0, 0x7F, _, 0x01, 0x01, hh, mm, ss, ff, 0xF7] = *bytes {
        return Some(Message::TimecodeFull {
            hours: hh & 0x1F,
            minutes: mm & 0x3F,
            seconds: ss & 0x3F,
            frames: ff & 0x1F,
            rate: (hh >> 5) & 0x03,
        });
    }
    let channel = (status & 0x0F) + 1;
    let data = |i: usize| bytes.get(i).copied().filter(|b| *b < 0x80);
    Some(match status & 0xF0 {
        0x80 => Message::NoteOff {
            channel,
            note: data(1)?,
        },
        0x90 => match (data(1)?, data(2)?) {
            (note, 0) => Message::NoteOff { channel, note },
            (note, velocity) => Message::NoteOn {
                channel,
                note,
                velocity,
            },
        },
        0xB0 => Message::Control {
            channel,
            controller: data(1)?,
            value: data(2)?,
        },
        0xC0 => Message::Program {
            channel,
            program: data(1)?,
        },
        0xD0 => Message::ChannelPressure {
            channel,
            value: data(1)?,
        },
        0xE0 => Message::PitchBend {
            channel,
            value: ((i16::from(data(2)?) << 7) | i16::from(data(1)?)) - 8192,
        },
        _ => return None,
    })
}

/// A control change, channel 1–16, as bytes.
pub fn control_change(channel: u8, controller: u8, value: u8) -> [u8; 3] {
    [
        0xB0 | (channel.clamp(1, 16) - 1),
        controller & 0x7F,
        value & 0x7F,
    ]
}

/// Picks the port whose name contains `wanted` (case-insensitive), or the
/// first port when `wanted` is empty.
fn choose<'a>(names: impl Iterator<Item = &'a str>, wanted: &str) -> Option<usize> {
    let wanted = wanted.to_lowercase();
    names
        .enumerate()
        .find(|(_, n)| wanted.is_empty() || n.to_lowercase().contains(&wanted))
        .map(|(i, _)| i)
}

#[derive(Debug, Default)]
struct InShared {
    queue: Mutex<Vec<Message>>,
    port: OnceLock<String>,
    error: OnceLock<String>,
}

/// Most queued messages kept if the cook stalls (oldest are dropped).
const MAX_QUEUE: usize = 4096;

impl InShared {
    fn push(&self, msg: Message) {
        if let Ok(mut q) = self.queue.lock() {
            if q.len() >= MAX_QUEUE {
                q.remove(0);
            }
            q.push(msg);
        }
    }
}

/// A MIDI input port.
#[derive(Debug)]
pub struct MidiIn {
    shared: Arc<InShared>,
    _stop: Option<Sender<()>>,
}

impl MidiIn {
    /// Opens the input whose name contains `device` ("" for the first).
    /// Never blocks; see [`Self::port`] and [`Self::error`].
    pub fn open(device: &str) -> Self {
        let shared = Arc::new(InShared::default());
        let (stop, stopped) = channel::<()>();
        let thread_shared = shared.clone();
        let device = device.to_owned();
        let spawned = std::thread::Builder::new()
            .name("atd-midi-in".into())
            .spawn(move || {
                if let Err(e) = run_in(&device, &thread_shared, &stopped) {
                    log::warn!("MIDI in: {e}");
                    let _ = thread_shared.error.set(e);
                }
            });
        if let Err(e) = spawned {
            let _ = shared.error.set(format!("cannot start MIDI thread: {e}"));
        }
        Self {
            shared,
            _stop: Some(stop),
        }
    }

    /// An input with no device; feed it with [`Self::push`].
    pub fn detached() -> Self {
        let shared = Arc::new(InShared::default());
        let _ = shared.port.set("detached".into());
        Self {
            shared,
            _stop: None,
        }
    }

    /// Queues a message as if it came from the device.
    pub fn push(&self, msg: Message) {
        self.shared.push(msg);
    }

    /// Moves queued messages into `out` (cleared first); never blocks.
    pub fn drain(&self, out: &mut Vec<Message>) {
        out.clear();
        if let Ok(mut q) = self.shared.queue.try_lock() {
            std::mem::swap(&mut *q, out);
        }
    }

    pub fn port(&self) -> Option<&str> {
        self.shared.port.get().map(String::as_str)
    }

    pub fn error(&self) -> Option<&str> {
        self.shared.error.get().map(String::as_str)
    }
}

fn run_in(device: &str, shared: &Arc<InShared>, stop: &Receiver<()>) -> Result<(), String> {
    let input = midir::MidiInput::new("Atidarla").map_err(|e| e.to_string())?;
    let ports = input.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|p| input.port_name(p).unwrap_or_default())
        .collect();
    let index = choose(names.iter().map(String::as_str), device).ok_or_else(|| {
        if device.is_empty() {
            "no MIDI input ports".to_owned()
        } else {
            format!("no MIDI input matching {device:?}")
        }
    })?;
    let callback_shared = shared.clone();
    let _connection = input
        .connect(
            &ports[index],
            "atidarla-in",
            move |_, bytes, _| {
                if let Some(msg) = parse_at(bytes, now_micros()) {
                    callback_shared.push(msg);
                }
            },
            (),
        )
        .map_err(|e| e.to_string())?;
    let _ = shared.port.set(names[index].clone());
    // Keep the connection open until the handle drops.
    let _ = stop.recv();
    Ok(())
}

/// Most messages a detached output keeps until they are read.
const MAX_RECORDED: usize = 65_536;

/// A short message on its way out: up to three bytes.
#[derive(Debug, Clone, Copy)]
struct Raw {
    bytes: [u8; 3],
    len: u8,
}

impl Raw {
    fn new(bytes: &[u8]) -> Self {
        let len = bytes.len().min(3);
        let mut b = [0; 3];
        b[..len].copy_from_slice(&bytes[..len]);
        Self {
            bytes: b,
            len: len as u8,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

#[derive(Debug, Default)]
struct OutShared {
    port: OnceLock<String>,
    error: OnceLock<String>,
    /// Detached outputs record what they would have sent, and when.
    sent: Mutex<Vec<(Raw, Instant)>>,
}

/// Where a [`MidiOut`]'s messages go; cloneable, so a timing thread (the
/// clock) can send on the same port as the cook.
#[derive(Debug, Clone)]
pub struct OutHandle {
    shared: Arc<OutShared>,
    tx: Option<Sender<Raw>>,
}

impl OutHandle {
    /// Queues up to three bytes (a short message) for sending; never blocks.
    pub fn send_raw(&self, bytes: &[u8]) {
        let msg = Raw::new(bytes);
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(msg);
            }
            None => {
                // A detached port is for tests and virtual routing; a clock
                // left running into one must not grow without bound.
                if let Ok(mut s) = self.shared.sent.lock()
                    && s.len() < MAX_RECORDED
                {
                    s.push((msg, Instant::now()));
                }
            }
        }
    }
}

/// A MIDI output port.
#[derive(Debug)]
pub struct MidiOut {
    handle: OutHandle,
}

impl MidiOut {
    /// Opens the output whose name contains `device` ("" for the first).
    pub fn open(device: &str) -> Self {
        let shared = Arc::new(OutShared::default());
        let (tx, rx) = channel::<Raw>();
        let thread_shared = shared.clone();
        let device = device.to_owned();
        let spawned = std::thread::Builder::new()
            .name("atd-midi-out".into())
            .spawn(move || {
                if let Err(e) = run_out(&device, &thread_shared, &rx) {
                    log::warn!("MIDI out: {e}");
                    let _ = thread_shared.error.set(e);
                }
            });
        if let Err(e) = spawned {
            let _ = shared.error.set(format!("cannot start MIDI thread: {e}"));
        }
        Self {
            handle: OutHandle {
                shared,
                tx: Some(tx),
            },
        }
    }

    /// An output with no device that records messages ([`Self::sent`]).
    pub fn detached() -> Self {
        let shared = Arc::new(OutShared::default());
        let _ = shared.port.set("detached".into());
        Self {
            handle: OutHandle { shared, tx: None },
        }
    }

    /// Queues a message for sending; never blocks.
    pub fn send(&self, msg: [u8; 3]) {
        self.handle.send_raw(&msg);
    }

    /// Queues up to three bytes (real-time and Song Position messages are
    /// shorter than a channel message); never blocks.
    pub fn send_raw(&self, bytes: &[u8]) {
        self.handle.send_raw(bytes);
    }

    /// A cloneable sender onto this port, for a timing thread.
    pub fn handle(&self) -> OutHandle {
        self.handle.clone()
    }

    /// Messages a detached output has recorded (drained), short ones padded
    /// with zeros.
    pub fn sent(&self) -> Vec<[u8; 3]> {
        self.drain_sent()
            .into_iter()
            .map(|(m, _)| m.bytes)
            .collect()
    }

    /// [`Self::sent`] with each message's exact bytes and when it was sent.
    pub fn sent_raw(&self) -> Vec<(Vec<u8>, Instant)> {
        self.drain_sent()
            .into_iter()
            .map(|(m, t)| (m.as_slice().to_vec(), t))
            .collect()
    }

    fn drain_sent(&self) -> Vec<(Raw, Instant)> {
        self.handle
            .shared
            .sent
            .lock()
            .map(|mut s| std::mem::take(&mut *s))
            .unwrap_or_default()
    }

    pub fn port(&self) -> Option<&str> {
        self.handle.shared.port.get().map(String::as_str)
    }

    pub fn error(&self) -> Option<&str> {
        self.handle.shared.error.get().map(String::as_str)
    }
}

fn run_out(device: &str, shared: &OutShared, rx: &Receiver<Raw>) -> Result<(), String> {
    let output = midir::MidiOutput::new("Atidarla").map_err(|e| e.to_string())?;
    let ports = output.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|p| output.port_name(p).unwrap_or_default())
        .collect();
    let index = choose(names.iter().map(String::as_str), device).ok_or_else(|| {
        if device.is_empty() {
            "no MIDI output ports".to_owned()
        } else {
            format!("no MIDI output matching {device:?}")
        }
    })?;
    let mut connection = output
        .connect(&ports[index], "atidarla-out")
        .map_err(|e| e.to_string())?;
    let _ = shared.port.set(names[index].clone());
    // Ends when the last sender (the handle and any clones) drops.
    while let Ok(msg) = rx.recv() {
        if let Err(e) = connection.send(msg.as_slice()) {
            log::warn!("MIDI out: {e}");
        }
    }
    Ok(())
}

/// Names of the MIDI input and output ports (blocks while the system
/// enumerates; call it off the render thread).
pub fn ports() -> (Vec<String>, Vec<String>) {
    let ins = midir::MidiInput::new("Atidarla")
        .map(|m| {
            m.ports()
                .iter()
                .filter_map(|p| m.port_name(p).ok())
                .collect()
        })
        .unwrap_or_default();
    let outs = midir::MidiOutput::new("Atidarla")
        .map(|m| {
            m.ports()
                .iter()
                .filter_map(|p| m.port_name(p).ok())
                .collect()
        })
        .unwrap_or_default();
    (ins, outs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel_voice_messages() {
        assert_eq!(
            parse(&[0x92, 60, 100]),
            Some(Message::NoteOn {
                channel: 3,
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(
            parse(&[0x90, 60, 0]),
            Some(Message::NoteOff {
                channel: 1,
                note: 60
            })
        );
        assert_eq!(
            parse(&[0xBF, 7, 127]),
            Some(Message::Control {
                channel: 16,
                controller: 7,
                value: 127
            })
        );
        assert_eq!(
            parse(&[0xE0, 0, 0x40]),
            Some(Message::PitchBend {
                channel: 1,
                value: 0
            })
        );
        assert_eq!(
            parse(&[0xE0, 0x7F, 0x7F]),
            Some(Message::PitchBend {
                channel: 1,
                value: 8191
            })
        );
        assert_eq!(parse(&[0xFE]), None, "active sensing is ignored");
        assert_eq!(parse_at(&[0xF8], 42), Some(Message::Clock { micros: 42 }));
        assert_eq!(parse(&[0xFA]), Some(Message::Start { micros: 0 }));
        assert_eq!(parse(&[0xFB]), Some(Message::Continue { micros: 0 }));
        assert_eq!(parse(&[0xFC]), Some(Message::Stop { micros: 0 }));
        assert_eq!(
            parse(&[0xF2, 0x01, 0x02]),
            Some(Message::SongPosition { sixteenths: 257 })
        );
        assert_eq!(parse(&[0xF2, 0x01]), None, "truncated");
        assert_eq!(
            parse(&[0xF1, 0x35]),
            Some(Message::QuarterFrame { piece: 3, value: 5 })
        );
        assert_eq!(
            parse(&[0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x20 | 13, 59, 30, 24, 0xF7]),
            Some(Message::TimecodeFull {
                hours: 13,
                minutes: 59,
                seconds: 30,
                frames: 24,
                rate: 1
            })
        );
        assert_eq!(parse(&[0xB0, 7]), None, "truncated");
        assert_eq!(parse(&[0xB0, 0x80, 1]), None, "data bytes are 7-bit");
        assert_eq!(control_change(1, 7, 64), [0xB0, 7, 64]);
        assert_eq!(control_change(16, 200, 255), [0xBF, 0x48, 0x7F]);
    }

    #[test]
    fn port_choice_by_substring() {
        let names = ["Midi Through", "nanoKONTROL2 CTRL", "IAC Bus 1"];
        assert_eq!(choose(names.iter().copied(), ""), Some(0));
        assert_eq!(choose(names.iter().copied(), "nanokontrol"), Some(1));
        assert_eq!(choose(names.iter().copied(), "bus"), Some(2));
        assert_eq!(choose(names.iter().copied(), "launchpad"), None);
    }

    #[test]
    fn detached_handles_queue_and_record() {
        let input = MidiIn::detached();
        input.push(Message::Program {
            channel: 1,
            program: 3,
        });
        let mut out = Vec::new();
        input.drain(&mut out);
        assert_eq!(
            out,
            [Message::Program {
                channel: 1,
                program: 3
            }]
        );
        input.drain(&mut out);
        assert!(out.is_empty());
        let output = MidiOut::detached();
        output.send([0xB0, 1, 2]);
        output.send_raw(&[0xF8]);
        assert_eq!(output.sent(), [[0xB0, 1, 2], [0xF8, 0, 0]]);
        output.send_raw(&[0xFA]);
        assert_eq!(output.sent_raw()[0].0, [0xFA], "exact length is kept");
    }

    #[test]
    fn opening_without_devices_reports_an_error_without_blocking() {
        let start = std::time::Instant::now();
        let input = MidiIn::open("surely no such port");
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        while input.error().is_none() && input.port().is_none() {
            assert!(start.elapsed().as_secs() < 10, "the open finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(input.error().is_some());
    }
}
