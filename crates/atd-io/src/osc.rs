//! OSC over UDP (PRD §5.2, FR-S5 groundwork).
//!
//! [`OscReceiver`] listens on a port in a background thread and keeps the
//! latest numeric value of every address. [`OscSender`] sends named values
//! from a background thread. The cook path only swaps values under a short
//! lock; all decoding, encoding and socket calls happen on the I/O threads.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rosc::{OscMessage, OscPacket, OscType};

/// How often I/O threads wake to check for shutdown.
const POLL: Duration = Duration::from_millis(50);

/// Channel name for an OSC address: `/mixer/fader1` → `mixer_fader1`.
pub fn channel_name(address: &str) -> String {
    let name = address.trim_start_matches('/').replace('/', "_");
    if name.is_empty() { "osc".into() } else { name }
}

fn number(arg: &OscType) -> Option<f32> {
    match arg {
        OscType::Float(f) => Some(*f),
        OscType::Double(d) => Some(*d as f32),
        OscType::Int(i) => Some(*i as f32),
        OscType::Long(l) => Some(*l as f32),
        OscType::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn store(packet: OscPacket, values: &Mutex<BTreeMap<String, f32>>) {
    match packet {
        OscPacket::Message(m) => {
            if let Some(v) = m.args.first().and_then(number) {
                values
                    .lock()
                    .expect("osc values")
                    .insert(channel_name(&m.addr), v);
            }
        }
        OscPacket::Bundle(b) => {
            for p in b.content {
                store(p, values);
            }
        }
    }
}

/// Receives OSC messages on a UDP port.
#[derive(Debug)]
pub struct OscReceiver {
    values: Arc<Mutex<BTreeMap<String, f32>>>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<JoinHandle<()>>,
}

impl OscReceiver {
    /// Listens on all interfaces at `port` (0 picks a free port; see [`Self::port`]).
    pub fn bind(port: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port))?;
        socket.set_read_timeout(Some(POLL))?;
        let port = socket.local_addr()?.port();
        let values = Arc::new(Mutex::new(BTreeMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (v, s) = (Arc::clone(&values), Arc::clone(&stop));
        let thread = std::thread::Builder::new()
            .name(format!("atd-osc-in-{port}"))
            .spawn(move || {
                let mut buf = [0u8; rosc::decoder::MTU];
                while !s.load(Ordering::Relaxed) {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _)) => match rosc::decoder::decode_udp(&buf[..n]) {
                            Ok((_, packet)) => store(packet, &v),
                            Err(e) => log::debug!("bad OSC packet: {e:?}"),
                        },
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => log::warn!("OSC receive failed: {e}"),
                    }
                }
            })?;
        Ok(Self {
            values,
            stop,
            port,
            thread: Some(thread),
        })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Runs `f` on the latest value of every received address, by channel name.
    pub fn with_values<R>(&self, f: impl FnOnce(&BTreeMap<String, f32>) -> R) -> R {
        f(&self.values.lock().expect("osc values"))
    }
}

impl Drop for OscReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Debug, Default)]
struct Outbox {
    entries: Vec<(String, f32)>,
    len: usize,
    dirty: bool,
    stop: bool,
}

/// Sends named values as OSC messages (`<prefix>/<name> <float>`).
#[derive(Debug)]
pub struct OscSender {
    shared: Arc<(Mutex<Outbox>, Condvar)>,
    thread: Option<JoinHandle<()>>,
    target: SocketAddr,
}

impl OscSender {
    pub fn new(host: &str, port: u16, prefix: &str) -> io::Result<Self> {
        let target = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve {host}"))
        })?;
        let socket = UdpSocket::bind(if target.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })?;
        let shared = Arc::new((Mutex::new(Outbox::default()), Condvar::new()));
        let sh = Arc::clone(&shared);
        let prefix = prefix.trim_end_matches('/').to_owned();
        let thread = std::thread::Builder::new()
            .name("atd-osc-out".into())
            .spawn(move || {
                let mut batch: Vec<(String, f32)> = Vec::new();
                loop {
                    {
                        let (lock, cv) = &*sh;
                        let mut out = lock.lock().expect("osc outbox");
                        while !out.dirty && !out.stop {
                            out = cv.wait_timeout(out, POLL).expect("osc outbox").0;
                        }
                        if out.stop {
                            return;
                        }
                        batch.clear();
                        batch.extend(out.entries[..out.len].iter().cloned());
                        out.dirty = false;
                    }
                    for (name, value) in &batch {
                        let msg = OscPacket::Message(OscMessage {
                            addr: format!("{prefix}/{name}"),
                            args: vec![OscType::Float(*value)],
                        });
                        match rosc::encoder::encode(&msg) {
                            Ok(bytes) => {
                                if let Err(e) = socket.send_to(&bytes, target) {
                                    log::warn!("OSC send to {target} failed: {e}");
                                }
                            }
                            Err(e) => log::warn!("OSC encode failed: {e:?}"),
                        }
                    }
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
            target,
        })
    }

    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// Queues the current values for sending. Reuses storage, so a steady
    /// set of names does not allocate on the caller's thread.
    pub fn publish<'a>(&self, values: impl IntoIterator<Item = (&'a str, f32)>) {
        let (lock, cv) = &*self.shared;
        let mut out = lock.lock().expect("osc outbox");
        let out = &mut *out;
        let mut n = 0;
        for (name, value) in values {
            if n < out.entries.len() {
                let e = &mut out.entries[n];
                if e.0 != name {
                    e.0.clear();
                    e.0.push_str(name);
                }
                e.1 = value;
            } else {
                out.entries.push((name.to_owned(), value));
            }
            n += 1;
        }
        out.len = n;
        out.dirty = true;
        cv.notify_one();
    }
}

impl Drop for OscSender {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        lock.lock().expect("osc outbox").stop = true;
        cv.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn names() {
        assert_eq!(channel_name("/mixer/fader1"), "mixer_fader1");
        assert_eq!(channel_name("/"), "osc");
    }

    #[test]
    fn sender_to_receiver_over_loopback() {
        let rx = OscReceiver::bind(0).unwrap();
        let tx = OscSender::new("127.0.0.1", rx.port(), "/atd/").unwrap();
        tx.publish([("level", 0.25), ("beat", 3.0)]);
        assert!(
            wait_for(|| rx.with_values(|v| v.len() == 2)),
            "both values arrive"
        );
        rx.with_values(|v| {
            assert_eq!(v.get("atd_level"), Some(&0.25));
            assert_eq!(v.get("atd_beat"), Some(&3.0));
        });
        tx.publish([("level", 0.75)]);
        assert!(wait_for(
            || rx.with_values(|v| v.get("atd_level") == Some(&0.75))
        ));
    }

    #[test]
    fn receiver_handles_ints_bools_and_bundles() {
        let rx = OscReceiver::bind(0).unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bundle = OscPacket::Bundle(rosc::OscBundle {
            timetag: rosc::OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: vec![
                OscPacket::Message(OscMessage {
                    addr: "/a".into(),
                    args: vec![OscType::Int(7)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/b".into(),
                    args: vec![OscType::Bool(true)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/text".into(),
                    args: vec![OscType::String("x".into())],
                }),
            ],
        });
        sock.send_to(
            &rosc::encoder::encode(&bundle).unwrap(),
            ("127.0.0.1", rx.port()),
        )
        .unwrap();
        assert!(
            wait_for(|| rx.with_values(|v| v.len() == 2)),
            "numeric arguments only"
        );
        rx.with_values(|v| assert_eq!((v.get("a"), v.get("b")), (Some(&7.0), Some(&1.0))));
    }

    #[test]
    fn dropping_stops_threads_promptly() {
        let start = Instant::now();
        drop(OscReceiver::bind(0).unwrap());
        drop(OscSender::new("127.0.0.1", 9, "/x").unwrap());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
