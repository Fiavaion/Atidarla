//! Client side of AI Host Protocol v1 (`docs/ai-host-protocol.md`): one
//! session with a host that is listening (the local host the supervisor
//! started, a LAN host or a cloud pod).
//!
//! Requests are pipelined: a reader thread matches responses to requests by
//! id, so several frames can be in flight while a `host.health` is answered.
//! Every call waits at most its timeout; a dead host surfaces as an error,
//! never a hang. Calls block their caller, so in the engine they run on AI
//! worker threads, never on the render thread (rule 1).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::egress;
use crate::protocol::{
    Capabilities, FrameCancel, FrameResult, FrameSubmit, Health, Incoming, ModelInfo, ModelLoad,
    ModelRef, PROTOCOL_VERSION, Request, RpcError, SessionInfo, SessionOpen, Transport,
};
use crate::ring::{FrameRing, RingLayout, default_ring_path};

/// Per-call timeout; a hung host surfaces as an error instead of a stall.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// `model.load` may read gigabytes of weights.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Notifications kept for the UI.
const NOTES: usize = 32;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ClientError {
    #[error("I/O error talking to AI host: {0}")]
    Io(String),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("AI host protocol error: {0}")]
    Protocol(String),
    #[error("AI host did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the AI host connection closed")]
    Disconnected,
    #[error(transparent)]
    Egress(Arc<egress::EgressError>),
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<egress::EgressError> for ClientError {
    fn from(e: egress::EgressError) -> Self {
        Self::Egress(Arc::new(e))
    }
}

impl ClientError {
    /// Whether the session is unusable (as opposed to one call failing).
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Disconnected | Self::Timeout(_) | Self::Egress(_)
        )
    }

    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Rpc(e) => Some(e.code),
            _ => None,
        }
    }
}

/// How to launch a local host process (the supervisor appends the listen,
/// token and parent arguments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl HostCommand {
    /// `python -m atd_ai_host` with `src` on `PYTHONPATH` (development layout).
    pub fn python_module(python: impl Into<OsString>, src: impl Into<PathBuf>) -> Self {
        Self {
            program: python.into(),
            args: vec!["-m".into(), "atd_ai_host".into()],
            env: vec![("PYTHONPATH".into(), src.into().into_os_string())],
        }
    }
}

/// How frames travel in a session being opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePath {
    /// A shared-memory ring of this capacity (local hosts only).
    Shm(RingLayout),
    /// Frames on the socket (LAN and cloud hosts).
    Inline,
}

type Reply = Result<(Value, Option<Vec<u8>>), ClientError>;

#[derive(Debug, Default)]
struct Shared {
    pending: Mutex<HashMap<u64, Sender<Reply>>>,
    notes: Mutex<Vec<(String, Value)>>,
    alive: AtomicBool,
}

/// A request in flight.
#[derive(Debug)]
pub struct Pending {
    rx: Receiver<Reply>,
    timeout: Duration,
}

impl Pending {
    /// Waits for the answer.
    pub fn wait(self) -> Reply {
        match self.rx.recv_timeout(self.timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err(ClientError::Timeout(self.timeout)),
            Err(RecvTimeoutError::Disconnected) => Err(ClientError::Disconnected),
        }
    }

    /// The answer if it has arrived.
    pub fn try_take(&self) -> Option<Reply> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(ClientError::Disconnected))
            }
        }
    }
}

/// One session with a listening host.
#[derive(Debug)]
pub struct HostClient {
    writer: Mutex<TcpStream>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    reader: Option<JoinHandle<()>>,
    ring: Option<FrameRing>,
    session: SessionInfo,
    peer: String,
}

impl HostClient {
    /// Connects to a host at `addr` (`host:port`) through the egress gate
    /// and opens a session.
    pub fn connect(addr: &str, token: Option<&str>, path: FramePath) -> Result<Self, ClientError> {
        let (host, port) = split_addr(addr)?;
        let stream = egress::connect(host, port, CALL_TIMEOUT)?;
        stream.set_nodelay(true)?;
        let shared = Arc::new(Shared {
            alive: AtomicBool::new(true),
            ..Shared::default()
        });
        let reader = {
            let stream = stream.try_clone()?;
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("atd-ai-reader".into())
                .spawn(move || read_loop(stream, &shared))?
        };
        let ring = match path {
            FramePath::Shm(layout) => Some(FrameRing::create(default_ring_path(), layout)?),
            FramePath::Inline => None,
        };
        let mut client = Self {
            writer: Mutex::new(stream),
            shared,
            next_id: AtomicU64::new(1),
            reader: Some(reader),
            ring,
            session: SessionInfo::default(),
            peer: addr.to_owned(),
        };
        let transport = match &client.ring {
            Some(ring) => Transport::Shm {
                ring_path: ring.path().to_string_lossy().into_owned(),
            },
            None => Transport::Inline {
                codecs: vec!["rgba8".into()],
            },
        };
        let open = SessionOpen {
            protocol_version: PROTOCOL_VERSION.into(),
            client: format!("atidarla {}", env!("CARGO_PKG_VERSION")),
            transport,
            token: token.map(str::to_owned),
        };
        client.session = client.call("session.open", &open)?;
        if client.session.protocol_version != PROTOCOL_VERSION {
            return Err(ClientError::Protocol(format!(
                "host speaks {}, this engine {PROTOCOL_VERSION}",
                client.session.protocol_version
            )));
        }
        Ok(client)
    }

    pub fn session(&self) -> &SessionInfo {
        &self.session
    }

    /// The address this session is connected to.
    pub fn peer(&self) -> &str {
        &self.peer
    }

    /// Whether the connection is still open.
    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
    }

    /// The shared-memory ring, on `shm` sessions.
    pub fn ring(&self) -> Option<&FrameRing> {
        self.ring.as_ref()
    }

    pub fn ring_mut(&mut self) -> Option<&mut FrameRing> {
        self.ring.as_mut()
    }

    /// Notifications received so far (oldest first), emptied.
    pub fn take_notes(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.shared.notes.lock().expect("notes"))
    }

    /// Sends a request (with an optional binary payload) without waiting.
    pub fn send<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        payload: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Pending, ClientError> {
        if !self.alive() {
            return Err(ClientError::Disconnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let mut request = Request::new(id, method, serde_json::to_value(params).map_err(proto)?);
        request.payload = payload.map(|p| p.len() as u64);
        let mut bytes = serde_json::to_vec(&request).map_err(proto)?;
        bytes.push(b'\n');
        let (tx, rx) = channel();
        self.shared.pending.lock().expect("pending").insert(id, tx);
        let written = {
            let mut w = self.writer.lock().expect("writer");
            w.write_all(&bytes)
                .and_then(|()| payload.map_or(Ok(()), |p| w.write_all(p)))
        };
        if let Err(e) = written {
            self.shared.pending.lock().expect("pending").remove(&id);
            return Err(e.into());
        }
        Ok(Pending { rx, timeout })
    }

    /// Sends one request and waits for its response.
    pub fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, ClientError> {
        self.call_timeout(method, params, CALL_TIMEOUT)
    }

    pub fn call_timeout<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<R, ClientError> {
        let (value, _) = self.send(method, params, None, timeout)?.wait()?;
        serde_json::from_value(value).map_err(proto)
    }

    pub fn capabilities(&self) -> Result<Capabilities, ClientError> {
        self.call("host.capabilities", &Value::Null)
    }

    pub fn health(&self) -> Result<Health, ClientError> {
        self.call("host.health", &Value::Null)
    }

    pub fn load_model(&self, load: &ModelLoad) -> Result<ModelInfo, ClientError> {
        self.call_timeout("model.load", load, LOAD_TIMEOUT)
    }

    /// Loads a built-in model (`builtin.invert`, `builtin.echo`).
    pub fn load_builtin(&self, model_id: &str) -> Result<ModelInfo, ClientError> {
        self.load_model(&ModelLoad {
            model_id: model_id.into(),
            adapter: "builtin".into(),
            path: None,
            options: Value::Null,
        })
    }

    pub fn unload_model(&self, model_id: &str) -> Result<(), ClientError> {
        let _: Value = self.call(
            "model.unload",
            &ModelRef {
                model_id: model_id.into(),
            },
        )?;
        Ok(())
    }

    /// Submits a frame. On `shm` sessions the input is already in its slot
    /// (stamped with `submit.seq`); on `inline` sessions `pixels` carries it.
    pub fn submit(
        &self,
        submit: &FrameSubmit,
        pixels: Option<&[u8]>,
    ) -> Result<Pending, ClientError> {
        self.send("frame.submit", submit, pixels, CALL_TIMEOUT)
    }

    /// Checks a frame answer: the result's seq and, on `shm`, the output
    /// slot's stamp must be the frame's, which catches stale or torn frames.
    pub fn check_result(
        &self,
        seq: u64,
        reply: (Value, Option<Vec<u8>>),
    ) -> Result<(FrameResult, Option<Vec<u8>>), ClientError> {
        let (value, payload) = reply;
        let result: FrameResult = serde_json::from_value(value).map_err(proto)?;
        let stamped = match (&self.ring, result.output_slot) {
            (Some(ring), Some(slot)) if slot < ring.layout().slot_count => ring.seq(slot) == seq,
            (Some(_), _) => false,
            (None, _) => payload.is_some(),
        };
        if result.seq != seq || !stamped {
            return Err(ClientError::Protocol(format!(
                "stale result for frame {seq}"
            )));
        }
        let want = result.width as usize * result.height as usize * 4;
        if let Some(p) = &payload
            && p.len() != want
        {
            return Err(ClientError::Protocol(format!(
                "frame {seq}: {} bytes for {}×{}",
                p.len(),
                result.width,
                result.height
            )));
        }
        Ok((result, payload))
    }

    pub fn cancel(&self, seq: u64) -> Result<Pending, ClientError> {
        self.send("frame.cancel", &FrameCancel { seq }, None, CALL_TIMEOUT)
    }

    /// Ends the session politely.
    pub fn close(self) -> Result<(), ClientError> {
        let _: Value = self.call("session.close", &Value::Null)?;
        Ok(())
    }
}

impl Drop for HostClient {
    fn drop(&mut self) {
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(Shutdown::Both);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Reads responses and notifications until the connection closes, then
/// fails every request still waiting.
fn read_loop(stream: TcpStream, shared: &Shared) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_str::<Incoming>(&line) else {
            log::warn!("AI host sent a malformed line");
            break;
        };
        let payload = match msg.payload {
            Some(n) => {
                let mut buf = vec![0; n as usize];
                if reader.read_exact(&mut buf).is_err() {
                    break;
                }
                Some(buf)
            }
            None => None,
        };
        match msg.id {
            Some(id) => {
                let tx = shared.pending.lock().expect("pending").remove(&id);
                if let Some(tx) = tx {
                    let reply = match msg.error {
                        Some(e) => Err(ClientError::Rpc(e)),
                        None => Ok((msg.result.unwrap_or(Value::Null), payload)),
                    };
                    let _ = tx.send(reply);
                }
            }
            None => {
                let mut notes = shared.notes.lock().expect("notes");
                if notes.len() >= NOTES {
                    notes.remove(0);
                }
                notes.push((msg.method.unwrap_or_default(), msg.params));
            }
        }
    }
    shared.alive.store(false, Ordering::Release);
    for (_, tx) in shared.pending.lock().expect("pending").drain() {
        let _ = tx.send(Err(ClientError::Disconnected));
    }
}

/// Splits `host:port` (IPv6 hosts in brackets).
pub fn split_addr(addr: &str) -> Result<(&str, u16), ClientError> {
    let bad = || ClientError::Protocol(format!("not a host:port address: {addr}"));
    let (host, port) = addr.rsplit_once(':').ok_or_else(bad)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = port.parse().map_err(|_| bad())?;
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host, port))
}

fn proto(e: serde_json::Error) -> ClientError {
    ClientError::Protocol(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_split() {
        assert_eq!(split_addr("127.0.0.1:7420").unwrap(), ("127.0.0.1", 7420));
        assert_eq!(split_addr("[::1]:80").unwrap(), ("::1", 80));
        assert_eq!(
            split_addr("studio.local:7420").unwrap(),
            ("studio.local", 7420)
        );
        assert!(split_addr("nohost").is_err());
        assert!(split_addr(":80").is_err());
    }
}
