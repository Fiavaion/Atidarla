//! WebSocket: a client connection (through the egress gate) on a worker
//! thread. Messages received become rows of the output table; each new
//! value on the `send` input is sent as one message. The cook only swaps
//! messages with the worker (CLAUDE.md rule 1).
//!
//! `wss://` is rustls (ring) over the same gated TCP connection, verified
//! against the operating system's trust store (`rustls-platform-verifier`,
//! as model downloads do): one trust policy for the app, the user's own and
//! enterprise roots work for LAN servers, and no root list ships with it.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lsr_core::dat::{Dat, Table};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{DatCtx, DatOp};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use tungstenite::{Message, WebSocket};

use crate::net::{UrlParts, lock, split_url};
use crate::{dat_in, op_schema};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a read waits before the worker looks at its outbox again.
const POLL: Duration = Duration::from_millis(20);
const RETRY: Duration = Duration::from_secs(1);
/// Messages waiting to be sent at most (older ones are dropped).
const MAX_OUTBOX: usize = 256;

pub fn schema() -> OperatorSchema {
    let mut s = op_schema(
        "websocket",
        "WebSocket",
        "A WebSocket client on a worker thread: messages it receives become rows of its output table (a `message` header row, then the newest `Keep` messages, newest last), and each new value on its `send` input is sent as one text message. Reconnects every second while Active and the connection is lost. `ws://` or `wss://` (TLS, the certificate checked against the operating system's trust store); either way the connection goes through Sovereign mode's egress gate: this machine and the LAN are always reachable, the internet only with Sovereign mode off.",
        vec![
            ParamSchema::string("url", "URL", "").help(
                "`ws://host:port/path` or `wss://host:port/path` of the server, for example `ws://127.0.0.1:8080/`.",
            ),
            ParamSchema::toggle("active", "Active", true)
                .help("Connects while on; off closes the connection."),
            ParamSchema::int("keep", "Keep", 20, Some(1), Some(1000))
                .help("Messages kept in the output table."),
        ],
        vec![dat_in("send", true)],
        40,
    );
    s.time_dependent = true;
    s.sink = true;
    s
}

#[derive(Debug, Default)]
struct State {
    received: VecDeque<String>,
    keep: usize,
    /// Bumped with every message received.
    seq: u64,
    outbox: VecDeque<String>,
    connected: bool,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
}

/// A connection's bytes: plain (`ws://`) or TLS (`wss://`).
enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(t) => &t.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(t) => t.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(t) => t.flush(),
        }
    }
}

/// The client TLS settings: rustls with the ring provider and the
/// operating system's trust store, built once.
fn tls_config() -> Result<Arc<ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let tls = |e: rustls::Error| format!("TLS: {e}");
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let builder = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(tls)?;
            #[cfg(not(test))]
            let builder = {
                use rustls_platform_verifier::BuilderVerifierExt;
                builder.with_platform_verifier().map_err(tls)?
            };
            // Tests: the same verifier, also trusting the local test CA.
            #[cfg(test)]
            let builder = {
                let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
                    [tests::test_ca().ca.clone()],
                    provider,
                )
                .map_err(tls)?;
                builder
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(verifier))
            };
            Ok(Arc::new(builder.with_no_client_auth()))
        })
        .clone()
}

fn open(url: &str, parts: &UrlParts) -> Result<WebSocket<Stream>, String> {
    // The egress gate first, for `ws://` and `wss://` alike.
    let tcp = lsr_ai::egress::connect(&parts.host, parts.port, CONNECT_TIMEOUT)
        .map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let stream = if parts.scheme == "wss" {
        let name = ServerName::try_from(parts.host.clone()).map_err(|e| format!("{url}: {e}"))?;
        let conn = ClientConnection::new(tls_config()?, name).map_err(|e| format!("{url}: {e}"))?;
        Stream::Tls(Box::new(StreamOwned::new(conn, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    let (ws, _) = tungstenite::client(url, stream).map_err(|e| format!("{url}: {e}"))?;
    ws.get_ref()
        .tcp()
        .set_read_timeout(Some(POLL))
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

fn session(ws: &mut WebSocket<Stream>, shared: &Shared) -> Result<(), String> {
    while !shared.stop.load(Ordering::Acquire) {
        let out: Vec<String> = lock(&shared.state).outbox.drain(..).collect();
        for m in out {
            ws.send(Message::text(m)).map_err(|e| e.to_string())?;
        }
        let text = match ws.read() {
            Ok(Message::Text(t)) => t.as_str().to_owned(),
            Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
            Ok(Message::Close(_)) => return Err("the server closed the connection".into()),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let mut s = lock(&shared.state);
        s.received.push_back(text);
        while s.received.len() > s.keep.max(1) {
            s.received.pop_front();
        }
        s.seq += 1;
    }
    let _ = ws.close(None);
    let _ = ws.flush();
    Ok(())
}

struct Connection {
    shared: Arc<Shared>,
}

impl Connection {
    fn start(url: String, parts: UrlParts, keep: usize) -> Self {
        let shared = Arc::new(Shared::default());
        lock(&shared.state).keep = keep;
        let s = shared.clone();
        std::thread::Builder::new()
            .name("lsr-websocket".into())
            .spawn(move || {
                while !s.stop.load(Ordering::Acquire) {
                    let r = open(&url, &parts).and_then(|mut ws| {
                        {
                            let mut st = lock(&s.state);
                            st.connected = true;
                            st.error = None;
                        }
                        session(&mut ws, &s)
                    });
                    {
                        let mut st = lock(&s.state);
                        st.connected = false;
                        if let Err(e) = r {
                            st.error = Some(e);
                        }
                    }
                    // Wait before reconnecting, stopping early if asked.
                    let mut waited = Duration::ZERO;
                    while waited < RETRY && !s.stop.load(Ordering::Acquire) {
                        std::thread::sleep(POLL);
                        waited += POLL;
                    }
                }
            })
            .expect("spawning a thread");
        Self { shared }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct WebSocketOp {
    conn: Option<Connection>,
    /// URL and Active the connection was made for.
    url: String,
    active: bool,
    error: Option<String>,
    sent: Option<Dat>,
    keep: i64,
    seen: Option<u64>,
    status: String,
}

impl std::fmt::Debug for WebSocketOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketOp")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl DatOp for WebSocketOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A sink offline (docs/render.md): no connection.
            return Ok(());
        }
        let url = p.str("url").trim();
        let active = p.bool("active");
        let keep = p.i64("keep").clamp(1, 1000);
        if url != self.url || active != self.active {
            self.url.clear();
            self.url.push_str(url);
            self.active = active;
            self.conn = None;
            self.error = None;
            self.seen = None;
            self.sent = None;
            self.status.clear();
            if active && !url.is_empty() {
                let parts = split_url(url, |s| match s {
                    "ws" => Some(80),
                    "wss" => Some(443),
                    _ => None,
                });
                match parts {
                    Ok(parts) if matches!(parts.scheme.as_str(), "ws" | "wss") => {
                        self.conn = Some(Connection::start(url.to_owned(), parts, keep as usize));
                        self.keep = keep;
                    }
                    Ok(parts) => {
                        self.error = Some(format!(
                            "`{url}`: unknown scheme `{}` (`ws://` or `wss://`)",
                            parts.scheme
                        ));
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        let Some(conn) = &self.conn else {
            if self.seen.is_none() {
                self.seen = Some(0);
                ctx.output
                    .replace(Dat::table(Table::from_rows([["message"]])));
            }
            return match &self.error {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            };
        };
        let send = ctx.inputs.get(0);
        let fresh = send.filter(|d| self.sent.as_ref().is_none_or(|old| !Dat::ptr_eq(old, d)));
        let Ok(mut s) = conn.shared.state.try_lock() else {
            return Ok(());
        };
        if let Some(d) = fresh {
            // The value there when wired or connected is sent too: a
            // WebSocket peer usually wants the current state.
            s.outbox.push_back(d.as_text().into_owned());
            while s.outbox.len() > MAX_OUTBOX {
                s.outbox.pop_front();
            }
            self.sent = Some(d.clone());
        }
        if keep != self.keep {
            s.keep = keep as usize;
            self.keep = keep;
        }
        if self.seen != Some(s.seq) {
            self.seen = Some(s.seq);
            let mut t = Table::new(1);
            t.push_row(["message"]);
            for m in s
                .received
                .iter()
                .skip(s.received.len().saturating_sub(keep as usize))
            {
                t.push_row([m.clone()]);
            }
            ctx.output.replace(Dat::table(t));
        }
        let (connected, error) = (s.connected, s.error.clone());
        drop(s);
        if connected {
            if !self.status.starts_with("connected") {
                self.status = format!("connected to {}", self.url);
            }
            Ok(())
        } else {
            if !self.status.starts_with("connecting") {
                self.status = format!("connecting to {}…", self.url);
            }
            match error {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn create() -> Box<dyn DatOp> {
    Box::<WebSocketOp>::default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::web::tests::until;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use serde_json::json;
    use std::net::{Ipv4Addr, TcpListener};

    /// A certificate authority made for the tests and a server certificate
    /// for `localhost` and 127.0.0.1 that it signed.
    pub(crate) struct TestCa {
        pub ca: CertificateDer<'static>,
        leaf: CertificateDer<'static>,
        key: Vec<u8>,
    }

    pub(crate) fn test_ca() -> &'static TestCa {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
            KeyPair, KeyUsagePurpose, SanType,
        };
        static CA: OnceLock<TestCa> = OnceLock::new();
        CA.get_or_init(|| {
            // Short validity: macOS refuses server certificates valid for
            // more than 825 days.
            let now = time::OffsetDateTime::now_utc();
            let valid = |p: &mut CertificateParams| {
                p.not_before = now - time::Duration::days(1);
                p.not_after = now + time::Duration::days(30);
            };
            let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
            ca.distinguished_name
                .push(DnType::CommonName, "Lasair test CA");
            ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            valid(&mut ca);
            let ca_key = KeyPair::generate().unwrap();
            let ca_cert = ca.self_signed(&ca_key).unwrap();
            let issuer = Issuer::new(ca, ca_key);
            let mut leaf = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
            leaf.subject_alt_names
                .push(SanType::IpAddress(Ipv4Addr::LOCALHOST.into()));
            leaf.distinguished_name
                .push(DnType::CommonName, "localhost");
            leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            valid(&mut leaf);
            let leaf_key = KeyPair::generate().unwrap();
            let leaf_cert = leaf.signed_by(&leaf_key, &issuer).unwrap();
            TestCa {
                ca: ca_cert.der().clone(),
                leaf: leaf_cert.der().clone(),
                key: leaf_key.serialize_der(),
            }
        })
    }

    /// The echo server's conversation, over any stream.
    fn echo<S: Read + Write>(mut ws: WebSocket<S>) {
        ws.send(Message::text("hello")).unwrap();
        while let Ok(m) = ws.read() {
            if let Message::Text(t) = m {
                ws.send(Message::text(format!("echo: {}", t.as_str())))
                    .unwrap();
            }
        }
    }

    /// A loopback server: greets, then echoes each message once.
    fn echo_server() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            echo(tungstenite::accept(stream).unwrap());
        });
        port
    }

    /// The same over TLS, with the test CA's certificate.
    fn tls_echo_server() -> u16 {
        let ca = test_ca();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ca.key.clone()));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![ca.leaf.clone(), ca.ca.clone()], key)
        .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let conn = rustls::ServerConnection::new(config).unwrap();
            echo(tungstenite::accept(StreamOwned::new(conn, stream)).unwrap());
        });
        port
    }

    #[test]
    fn wss_connects_over_tls_and_echoes() {
        let port = tls_echo_server();
        let mut h = Harness::new(schema());
        let mut op = create();
        h.set("url", json!(format!("wss://127.0.0.1:{port}/live")));
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 2).unwrap();
        assert_eq!(h.out.cell(1, 0), Some("hello"));
        assert!(op.status().unwrap().starts_with("connected"));
        h.input(0, Some(Dat::text("cue 4")));
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 3).unwrap();
        assert_eq!(h.out.cell(2, 0), Some("echo: cue 4"));
    }

    #[test]
    fn wss_refuses_a_certificate_it_does_not_trust() {
        // A server whose certificate no trusted CA signed.
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                let conn = rustls::ServerConnection::new(config.clone()).unwrap();
                let _ = tungstenite::accept(StreamOwned::new(conn, stream));
            }
        });
        let mut h = Harness::new(schema());
        let mut op = create();
        h.set("url", json!(format!("wss://localhost:{port}/")));
        let r = until(&mut h, op.as_mut(), |_, r| r.is_err());
        let e = r.unwrap_err();
        assert!(e.contains("certificate"), "{e}");
        assert_eq!(h.out.rows(), 1, "nothing received");
    }

    #[test]
    fn messages_come_in_as_rows_and_new_values_go_out() {
        let port = echo_server();
        let mut h = Harness::new(schema());
        let mut op = create();
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "message", "no URL: just the header");
        h.set("url", json!(format!("ws://127.0.0.1:{port}/live")));
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 2).unwrap();
        assert_eq!(h.out.cell(1, 0), Some("hello"));
        assert!(op.status().unwrap().starts_with("connected"));
        h.input(0, Some(Dat::text("cue 4")));
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 3).unwrap();
        assert_eq!(h.out.cell(2, 0), Some("echo: cue 4"));
        // The same value is not sent again.
        for _ in 0..30 {
            h.cook(op.as_mut()).unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(h.out.rows(), 3);
        h.set("keep", json!(1));
        h.input(0, Some(Dat::text("cue 5")));
        until(&mut h, op.as_mut(), |h, _| {
            h.out.cell(1, 0) == Some("echo: cue 5")
        })
        .unwrap();
        assert_eq!(h.out.rows(), 2);
        // Inactive: disconnected, nothing kept but the header.
        h.set("active", json!(false));
        h.cook(op.as_mut()).unwrap();
        assert!(op.status().is_none());
    }

    #[test]
    fn bad_urls_and_unreachable_servers_are_errors() {
        let mut h = Harness::new(schema());
        let mut op = create();
        h.set("url", json!("http://127.0.0.1/"));
        assert!(h.cook(op.as_mut()).is_err());
        h.set("url", json!("http://127.0.0.1:80/"));
        assert!(h.cook(op.as_mut()).unwrap_err().contains("unknown scheme"));
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        h.set("url", json!(format!("ws://127.0.0.1:{free}/")));
        let r = until(&mut h, op.as_mut(), |_, r| r.is_err());
        assert!(r.unwrap_err().contains("127.0.0.1"));
        assert!(op.status().unwrap().starts_with("connecting"));
        // Sovereign mode (on by default) refuses the internet.
        h.set("url", json!("ws://203.0.113.9/"));
        let r = until(&mut h, op.as_mut(), |_, r| {
            r.as_ref().is_err_and(|e| e.contains("Sovereign"))
        });
        assert!(r.is_err());
        // The same for TLS: the gate refuses before any handshake.
        h.set("url", json!("wss://203.0.113.9/"));
        let r = until(&mut h, op.as_mut(), |_, r| {
            r.as_ref().is_err_and(|e| e.contains("Sovereign"))
        });
        assert!(r.is_err());
    }
}
