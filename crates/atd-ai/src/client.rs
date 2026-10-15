//! Client side of the AI Host Protocol prototype: spawns a host process,
//! accepts its control connection, and exchanges frames through a
//! [`FrameRing`].
//!
//! Calls are blocking. In the engine they run on the supervisor's I/O
//! thread, never on the render thread.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::protocol::{
    Capabilities, FrameResult, FrameSubmit, Health, ModelInfo, ModelRef, PROTOCOL_VERSION, Request,
    Response, RpcError, SessionInfo, SessionOpen,
};
use crate::ring::{FrameRing, RingLayout, default_ring_path};

/// How long to wait for a spawned host to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Per-call read timeout; a hung host surfaces as an error instead of a stall.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("I/O error talking to AI host: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("AI host protocol error: {0}")]
    Protocol(String),
    #[error("AI host did not connect within {0:?}")]
    ConnectTimeout(Duration),
    #[error("AI host exited before connecting ({0})")]
    HostExited(std::process::ExitStatus),
}

/// How to launch a host process. `--connect <addr>` is appended.
#[derive(Debug, Clone)]
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

/// A connected AI Host session.
#[derive(Debug)]
pub struct HostClient {
    child: Child,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    ring: FrameRing,
    next_id: u64,
    session: SessionInfo,
    line: String,
}

impl HostClient {
    /// Creates a ring, spawns the host, waits for it to connect and opens a session.
    pub fn spawn(cmd: &HostCommand, layout: RingLayout) -> Result<Self, ClientError> {
        let ring = FrameRing::create(default_ring_path(), layout)?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;

        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .arg("--connect")
            .arg(addr.to_string())
            .stdin(Stdio::null());
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
        let mut child = command.spawn()?;

        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.try_wait()? {
                        return Err(ClientError::HostExited(status));
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        return Err(ClientError::ConnectTimeout(CONNECT_TIMEOUT));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
        };
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(CALL_TIMEOUT))?;

        let writer = stream.try_clone()?;
        let mut client = Self {
            child,
            reader: BufReader::new(stream),
            writer,
            ring,
            next_id: 1,
            session: SessionInfo {
                session_id: String::new(),
                protocol_version: String::new(),
                host_name: String::new(),
                host_version: String::new(),
            },
            line: String::new(),
        };
        let open = SessionOpen {
            protocol_version: PROTOCOL_VERSION.into(),
            client: format!("atidarla {}", env!("CARGO_PKG_VERSION")),
            ring_path: client.ring.path().to_string_lossy().into_owned(),
        };
        client.session = client.call("session.open", &open)?;
        Ok(client)
    }

    pub fn session(&self) -> &SessionInfo {
        &self.session
    }

    pub fn ring(&self) -> &FrameRing {
        &self.ring
    }

    pub fn ring_mut(&mut self) -> &mut FrameRing {
        &mut self.ring
    }

    /// OS process ID of the host, for supervision and crash tests.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Sends one request and waits for its response.
    pub fn call<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<R, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request::new(id, method, serde_json::to_value(params).map_err(proto)?);
        let mut bytes = serde_json::to_vec(&request).map_err(proto)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes)?;

        self.line.clear();
        if self.reader.read_line(&mut self.line)? == 0 {
            return Err(ClientError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        let response: Response = serde_json::from_str(&self.line).map_err(proto)?;
        if response.id != id {
            return Err(ClientError::Protocol(format!(
                "response id {} for request {id}",
                response.id
            )));
        }
        if let Some(error) = response.error {
            return Err(error.into());
        }
        serde_json::from_value(response.result.unwrap_or(Value::Null)).map_err(proto)
    }

    pub fn capabilities(&mut self) -> Result<Capabilities, ClientError> {
        self.call("host.capabilities", &Value::Null)
    }

    pub fn health(&mut self) -> Result<Health, ClientError> {
        self.call("host.health", &Value::Null)
    }

    pub fn load_model(&mut self, model_id: &str) -> Result<ModelInfo, ClientError> {
        self.call(
            "model.load",
            &ModelRef {
                model_id: model_id.into(),
            },
        )
    }

    /// Submits the frame already written to `input_slot` and waits for the
    /// result in `output_slot`. Verifies the output slot's sequence stamp.
    pub fn process(&mut self, submit: &FrameSubmit) -> Result<FrameResult, ClientError> {
        let result: FrameResult = self.call("frame.submit", submit)?;
        if result.seq != submit.seq || self.ring.seq(result.output_slot) != submit.seq {
            return Err(ClientError::Protocol(format!(
                "stale result for frame {}",
                submit.seq
            )));
        }
        Ok(result)
    }

    /// Ends the session and waits for the host to exit.
    pub fn close(mut self) -> Result<std::process::ExitStatus, ClientError> {
        let _: Value = self.call("session.close", &json!(null))?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                self.child.kill()?;
                return Ok(self.child.wait()?);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Kills the host process (crash simulation; PT-9 groundwork).
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()?;
        self.child.wait().map(|_| ())
    }
}

impl Drop for HostClient {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn proto(e: serde_json::Error) -> ClientError {
    ClientError::Protocol(e.to_string())
}
