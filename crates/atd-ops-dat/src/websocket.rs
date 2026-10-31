//! WebSocket: a client connection (through the egress gate) on a worker
//! thread. Messages received become rows of the output table; each new
//! value on the `send` input is sent as one message. The cook only swaps
//! messages with the worker (CLAUDE.md rule 1).

use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atd_core::dat::{Dat, Table};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{DatCtx, DatOp};
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
        "A WebSocket client on a worker thread: messages it receives become rows of its output table (a `message` header row, then the newest `Keep` messages, newest last), and each new value on its `send` input is sent as one text message. Reconnects every second while Active and the connection is lost. `ws://` only (no TLS yet); the connection goes through Sovereign mode's egress gate: this machine and the LAN are always reachable, the internet only with Sovereign mode off.",
        vec![
            ParamSchema::string("url", "URL", "")
                .help("`ws://host:port/path` of the server, for example `ws://127.0.0.1:8080/`."),
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

fn open(url: &str, parts: &UrlParts) -> Result<WebSocket<TcpStream>, String> {
    let stream = atd_ai::egress::connect(&parts.host, parts.port, CONNECT_TIMEOUT)
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let (ws, _) = tungstenite::client(url, stream).map_err(|e| format!("{url}: {e}"))?;
    ws.get_ref()
        .set_read_timeout(Some(POLL))
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

fn session(ws: &mut WebSocket<TcpStream>, shared: &Shared) -> Result<(), String> {
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
            .name("atd-websocket".into())
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
                    Ok(parts) if parts.scheme == "ws" => {
                        self.conn = Some(Connection::start(url.to_owned(), parts, keep as usize));
                        self.keep = keep;
                    }
                    Ok(_) => {
                        self.error = Some(
                            "wss:// is not supported yet: use ws:// on this machine or the LAN"
                                .into(),
                        );
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
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::web::tests::until;
    use serde_json::json;
    use std::net::{Ipv4Addr, TcpListener};

    /// A loopback server: greets, then echoes each message once.
    fn echo_server() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(Message::text("hello")).unwrap();
            while let Ok(m) = ws.read() {
                if let Message::Text(t) = m {
                    ws.send(Message::text(format!("echo: {}", t.as_str())))
                        .unwrap();
                }
            }
        });
        port
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
        h.set("url", json!("wss://127.0.0.1/"));
        assert!(h.cook(op.as_mut()).unwrap_err().contains("wss"));
        h.set("url", json!("http://127.0.0.1/"));
        assert!(h.cook(op.as_mut()).is_err());
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
    }
}
