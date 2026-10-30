//! The one gate every AI connection goes through (FR-A8, Sovereign mode).
//!
//! Hosts, cloud adapters, ComfyUI and model downloads resolve their
//! destination with [`resolve`] (or open it with [`connect`]) and connect
//! only to the addresses it returns. Addresses are classified by IP, after
//! DNS, so a `.local` name or a LAN hostname counts as LAN and a public name
//! counts as internet whatever it is called. In Sovereign mode internet
//! addresses are refused before any packet is sent.
//!
//! Sovereign mode is process-wide: one project is open at a time and the
//! app sets it from the project on load (default on for new projects).

use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Where an address is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// This machine.
    Local,
    /// Private, link-local or carrier-grade NAT ranges (a Tailscale-style
    /// overlay counts as LAN: it is the user's own network).
    Lan,
    Internet,
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("Sovereign mode blocks {host}: it is on the internet, not this machine or the LAN")]
    Sovereign { host: String },
    #[error("could not resolve {host}: {source}")]
    Resolve { host: String, source: io::Error },
    #[error("{host} resolved to no usable address")]
    NoAddress { host: String },
    #[error("could not connect to {host}: {source}")]
    Connect { host: String, source: io::Error },
}

static SOVEREIGN: AtomicBool = AtomicBool::new(true);
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// Turns Sovereign mode on or off for the process.
pub fn set_sovereign(on: bool) {
    SOVEREIGN.store(on, Ordering::Release);
}

pub fn sovereign() -> bool {
    SOVEREIGN.load(Ordering::Acquire)
}

/// Connections refused by Sovereign mode since start (for the UI and tests).
pub fn refused() -> u64 {
    REFUSED.load(Ordering::Acquire)
}

/// Classifies an address.
pub fn classify(ip: IpAddr) -> Scope {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if v4.is_loopback() {
                Scope::Local
            } else if v4.is_private() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
            {
                Scope::Lan
            } else {
                Scope::Internet
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return classify(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            if v6.is_loopback() {
                Scope::Local
            } else if first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80 {
                Scope::Lan
            } else {
                Scope::Internet
            }
        }
    }
}

/// Whether `ip` may be reached under the current mode.
pub fn allowed(ip: IpAddr) -> bool {
    !sovereign() || classify(ip) != Scope::Internet
}

/// Resolves `host:port` and returns the addresses that may be reached, or
/// an error naming why none may. Callers connect only to these addresses
/// (never re-resolve), so DNS cannot swap in another destination.
pub fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, EgressError> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|source| EgressError::Resolve {
            host: host.to_owned(),
            source,
        })?
        .filter(|a| !a.ip().is_unspecified())
        .collect();
    if addrs.is_empty() {
        return Err(EgressError::NoAddress {
            host: host.to_owned(),
        });
    }
    let ok: Vec<SocketAddr> = addrs.into_iter().filter(|a| allowed(a.ip())).collect();
    if ok.is_empty() {
        REFUSED.fetch_add(1, Ordering::AcqRel);
        log::warn!("Sovereign mode refused a connection to {host}");
        return Err(EgressError::Sovereign {
            host: host.to_owned(),
        });
    }
    Ok(ok)
}

/// Opens a TCP connection to `host:port` through the gate.
pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, EgressError> {
    let mut last = None;
    for addr in resolve(host, port)? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(EgressError::Connect {
        host: host.to_owned(),
        source: last.unwrap_or_else(|| io::Error::other("no address")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn addresses_are_classified() {
        for (a, want) in [
            ("127.0.0.1", Scope::Local),
            ("::1", Scope::Local),
            ("10.1.2.3", Scope::Lan),
            ("172.20.0.1", Scope::Lan),
            ("192.168.1.10", Scope::Lan),
            ("169.254.3.4", Scope::Lan),
            ("100.101.102.103", Scope::Lan),
            ("fd12:3456::1", Scope::Lan),
            ("fe80::1", Scope::Lan),
            ("::ffff:192.168.0.2", Scope::Lan),
            ("8.8.8.8", Scope::Internet),
            ("203.0.113.9", Scope::Internet),
            ("172.32.0.1", Scope::Internet),
            ("100.128.0.1", Scope::Internet),
            ("2606:4700::1111", Scope::Internet),
            ("::ffff:8.8.8.8", Scope::Internet),
        ] {
            assert_eq!(classify(ip(a)), want, "{a}");
        }
    }

    // One test touches the process-wide flag, so tests cannot race on it.
    #[test]
    fn sovereign_mode_refuses_the_internet_before_connecting() {
        set_sovereign(true);
        let before = refused();
        let err = connect("203.0.113.9", 443, Duration::from_millis(50)).unwrap_err();
        assert!(matches!(err, EgressError::Sovereign { .. }), "{err}");
        assert_eq!(refused(), before + 1);
        // Loopback and the LAN stay reachable.
        assert!(resolve("127.0.0.1", 80).is_ok());
        assert!(resolve("192.168.1.10", 80).is_ok());
        set_sovereign(false);
        assert!(resolve("203.0.113.9", 443).is_ok());
        set_sovereign(true);
    }
}
