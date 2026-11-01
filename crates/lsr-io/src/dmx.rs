//! DMX512 over Ethernet: Art-Net (ArtDmx) and sACN (ANSI E1.31).
//!
//! [`DmxSender`] owns a UDP socket on its own thread. The cook publishes
//! the latest universe (a short lock, no I/O, rule 1). The thread sends
//! it when it changes, at most [`MAX_RATE`] times a second, and repeats it
//! every second as a keep-alive so receivers do not time out.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Highest send rate (DMX512's refresh rate at full universe size).
pub const MAX_RATE: f64 = 44.0;
const KEEPALIVE: Duration = Duration::from_secs(1);

pub const ARTNET_PORT: u16 = 6454;
pub const SACN_PORT: u16 = 5568;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    ArtNet,
    Sacn,
}

/// An ArtDmx packet (Art-Net 4) for a 15-bit port address.
pub fn artnet_dmx(universe: u16, sequence: u8, data: &[u8]) -> Vec<u8> {
    // Length must be even, 2–512.
    let n = data.len().clamp(2, 512).next_multiple_of(2).min(512);
    let mut p = Vec::with_capacity(18 + n);
    p.extend_from_slice(b"Art-Net\0");
    p.extend_from_slice(&0x5000u16.to_le_bytes()); // OpDmx
    p.extend_from_slice(&[0, 14]); // protocol version 14
    p.push(sequence);
    p.push(0); // physical port
    p.push((universe & 0xFF) as u8); // SubUni
    p.push(((universe >> 8) & 0x7F) as u8); // Net
    p.extend_from_slice(&(n as u16).to_be_bytes());
    p.extend_from_slice(&data[..data.len().min(n)]);
    p.resize(18 + n, 0);
    p
}

/// An E1.31 data packet. `cid` identifies this source; `name` is shown by
/// receivers.
pub fn sacn_data(universe: u16, sequence: u8, cid: &[u8; 16], name: &str, data: &[u8]) -> Vec<u8> {
    let n = data.len().min(512);
    let len = 126 + n;
    let flags_len = |l: usize| (0x7000 | (l as u16 & 0x0FFF)).to_be_bytes();
    let mut p = Vec::with_capacity(len);
    // Root layer.
    p.extend_from_slice(&0x0010u16.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(b"ASC-E1.17\0\0\0");
    p.extend_from_slice(&flags_len(len - 16));
    p.extend_from_slice(&4u32.to_be_bytes()); // VECTOR_ROOT_E131_DATA
    p.extend_from_slice(cid);
    // Framing layer.
    p.extend_from_slice(&flags_len(len - 38));
    p.extend_from_slice(&2u32.to_be_bytes()); // VECTOR_E131_DATA_PACKET
    let mut source = [0u8; 64];
    let bytes = name.as_bytes();
    let take = bytes.len().min(63);
    source[..take].copy_from_slice(&bytes[..take]);
    p.extend_from_slice(&source);
    p.push(100); // priority
    p.extend_from_slice(&0u16.to_be_bytes()); // sync address
    p.push(sequence);
    p.push(0); // options
    p.extend_from_slice(&universe.to_be_bytes());
    // DMP layer.
    p.extend_from_slice(&flags_len(len - 115));
    p.push(0x02); // VECTOR_DMP_SET_PROPERTY
    p.push(0xA1);
    p.extend_from_slice(&0u16.to_be_bytes()); // first address
    p.extend_from_slice(&1u16.to_be_bytes()); // increment
    p.extend_from_slice(&((n + 1) as u16).to_be_bytes());
    p.push(0); // DMX start code
    p.extend_from_slice(&data[..n]);
    debug_assert_eq!(p.len(), len);
    p
}

/// The default destination: Art-Net broadcast, or the sACN multicast
/// group for the universe.
pub fn default_target(protocol: Protocol, universe: u16) -> SocketAddr {
    match protocol {
        Protocol::ArtNet => SocketAddr::from((Ipv4Addr::BROADCAST, ARTNET_PORT)),
        Protocol::Sacn => {
            let [hi, lo] = universe.to_be_bytes();
            SocketAddr::from((Ipv4Addr::new(239, 255, hi, lo), SACN_PORT))
        }
    }
}

#[derive(Debug, Default)]
struct Outbox {
    data: Vec<u8>,
    dirty: bool,
    stop: bool,
}

/// Sends one universe from its own thread.
#[derive(Debug)]
pub struct DmxSender {
    shared: Arc<(Mutex<Outbox>, Condvar)>,
    thread: Option<JoinHandle<()>>,
    pub target: SocketAddr,
}

impl Drop for DmxSender {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut out) = lock.lock() {
            out.stop = true;
        }
        cv.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl DmxSender {
    /// Sends `universe` with `protocol` to `host` ("" for the default:
    /// broadcast or multicast), on the protocol's port unless `port` is set.
    pub fn new(
        protocol: Protocol,
        host: &str,
        port: Option<u16>,
        universe: u16,
    ) -> io::Result<Self> {
        let mut target = if host.is_empty() {
            default_target(protocol, universe)
        } else {
            let p = match protocol {
                Protocol::ArtNet => ARTNET_PORT,
                Protocol::Sacn => SACN_PORT,
            };
            (host, p).to_socket_addrs()?.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve {host}"))
            })?
        };
        if let Some(p) = port {
            target.set_port(p);
        }
        let socket = UdpSocket::bind(crate::source_addr(target))?;
        socket.set_broadcast(true)?;
        let shared = Arc::new((Mutex::new(Outbox::default()), Condvar::new()));
        let sh = Arc::clone(&shared);
        // A stable source ID for sACN, from the process and the clock.
        let mut cid = [0u8; 16];
        let seed = std::process::id() as u128
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
        cid.copy_from_slice(&seed.to_le_bytes());
        let thread = std::thread::Builder::new()
            .name("lsr-dmx-out".into())
            .spawn(move || {
                let min_gap = Duration::from_secs_f64(1.0 / MAX_RATE);
                let mut last_sent: Option<Instant> = None;
                let mut frame = Vec::new();
                let mut sequence: u8 = 0;
                loop {
                    {
                        let (lock, cv) = &*sh;
                        let Ok(mut out) = lock.lock() else { return };
                        let wait = |out: &Outbox| !out.dirty && !out.stop;
                        if wait(&out) {
                            let timeout = match last_sent {
                                Some(t) => KEEPALIVE.saturating_sub(t.elapsed()),
                                None => KEEPALIVE,
                            };
                            out = match cv.wait_timeout_while(out, timeout, |o| wait(o)) {
                                Ok((g, _)) => g,
                                Err(_) => return,
                            };
                        }
                        if out.stop {
                            return;
                        }
                        if out.dirty {
                            frame.clear();
                            frame.extend_from_slice(&out.data);
                            out.dirty = false;
                        }
                    }
                    if frame.is_empty() {
                        continue;
                    }
                    if let Some(t) = last_sent
                        && t.elapsed() < min_gap
                    {
                        std::thread::sleep(min_gap - t.elapsed());
                    }
                    sequence = sequence.wrapping_add(1);
                    // Art-Net reserves sequence 0 for "disabled".
                    if sequence == 0 {
                        sequence = 1;
                    }
                    let packet = match protocol {
                        Protocol::ArtNet => artnet_dmx(universe, sequence, &frame),
                        Protocol::Sacn => sacn_data(universe, sequence, &cid, "Lasair", &frame),
                    };
                    if let Err(e) = socket.send_to(&packet, target) {
                        log::warn!("DMX out to {target}: {e}");
                    }
                    last_sent = Some(Instant::now());
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
            target,
        })
    }

    /// Replaces the universe's slot values (up to 512); sent soon after.
    pub fn publish(&self, data: &[u8]) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut out) = lock.lock()
            && out.data != data
        {
            out.data.clear();
            out.data.extend_from_slice(&data[..data.len().min(512)]);
            out.dirty = true;
            cv.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artnet_packet_layout() {
        let p = artnet_dmx(0x123, 7, &[1, 2, 3]);
        assert_eq!(&p[..8], b"Art-Net\0");
        assert_eq!(&p[8..10], &[0x00, 0x50], "OpDmx, little-endian");
        assert_eq!(&p[10..12], &[0, 14]);
        assert_eq!(
            (p[12], p[14], p[15]),
            (7, 0x23, 0x01),
            "sequence, SubUni, Net"
        );
        assert_eq!(&p[16..18], &[0, 4], "length rounded up to even, big-endian");
        assert_eq!(&p[18..], &[1, 2, 3, 0]);
        assert_eq!(artnet_dmx(0, 1, &[]).len(), 20, "at least two slots");
    }

    #[test]
    fn sacn_packet_layout() {
        let cid = [9u8; 16];
        let p = sacn_data(3, 42, &cid, "Lasair", &[10, 20]);
        assert_eq!(p.len(), 128);
        assert_eq!(&p[4..16], b"ASC-E1.17\0\0\0");
        assert_eq!(u16::from_be_bytes([p[16], p[17]]), 0x7000 | 112);
        assert_eq!(&p[22..38], &cid);
        assert_eq!(u16::from_be_bytes([p[38], p[39]]), 0x7000 | 90);
        assert_eq!(&p[44..52], b"Lasair\0\0");
        assert_eq!((p[108], p[111]), (100, 42), "priority, sequence");
        assert_eq!(u16::from_be_bytes([p[113], p[114]]), 3, "universe");
        assert_eq!(u16::from_be_bytes([p[115], p[116]]), 0x7000 | 13);
        assert_eq!(
            u16::from_be_bytes([p[123], p[124]]),
            3,
            "start code + 2 slots"
        );
        assert_eq!(&p[125..], &[0, 10, 20]);
        assert_eq!(
            default_target(Protocol::Sacn, 0x0102),
            "239.255.1.2:5568".parse().unwrap()
        );
    }

    #[test]
    fn sender_delivers_and_keeps_alive() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let port = rx.local_addr().unwrap().port();
        let tx = DmxSender::new(Protocol::ArtNet, "127.0.0.1", Some(port), 1).unwrap();
        tx.publish(&[255, 128]);
        let mut buf = [0u8; 600];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[18..n], &[255, 128]);
        // Unchanged data is repeated as a keep-alive after about a second.
        let start = Instant::now();
        let n = rx.recv(&mut buf).unwrap();
        assert!(start.elapsed() > Duration::from_millis(500));
        assert_eq!(&buf[18..n], &[255, 128]);
        // sACN to the same receiver.
        drop(tx);
        let tx = DmxSender::new(Protocol::Sacn, "127.0.0.1", Some(port), 7).unwrap();
        tx.publish(&[1, 2, 3]);
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(n, 129);
        assert_eq!(&buf[126..n], &[1, 2, 3]);
    }
}
