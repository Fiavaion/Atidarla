//! AI Host Protocol v1: message types (`docs/ai-host-protocol.md`).
//!
//! Control messages are JSON-RPC 2.0 objects, one per line (`\n`), over a
//! stream socket. A message may carry a binary payload: `"payload": n` at
//! its top level and exactly `n` raw bytes after the newline (frames on the
//! `inline` transport). Requests may be pipelined; responses are matched by
//! id. Hosts may send notifications (no id).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version negotiated in `session.open`.
pub const PROTOCOL_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// Bytes of binary payload following the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<u64>,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
            payload: None,
        }
    }
}

/// Anything a host sends: a response (with `id`) or a notification.
#[derive(Debug, Clone, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
    #[serde(default)]
    pub payload: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("host error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// How frames travel in a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Transport {
    /// A shared-memory ring on this machine.
    Shm { ring_path: String },
    /// Pixels on the socket, in one of these codecs (preferred first).
    Inline { codecs: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOpen {
    pub protocol_version: String,
    pub client: String,
    pub transport: Transport,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub protocol_version: String,
    pub host_name: String,
    pub host_version: String,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub vendor: String,
    #[serde(default)]
    pub vram_mb: Option<u64>,
}

/// What a host can run; used by the compute router (FR-A7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// `nvidia`, `amd`, `intel`, `apple`, `cpu`.
    pub vendor: String,
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub vram_mb: Option<u64>,
    /// e.g. `torch-cuda`, `torch-mps`, `tensorrt`, `onnxruntime-coreml`, `cpu-reference`.
    #[serde(default)]
    pub runtimes: Vec<String>,
    /// `shm`, `inline`.
    #[serde(default)]
    pub transports: Vec<String>,
    /// Adapter kinds: `builtin`, `diffusion.sd`, `onnx`, `video.v2v`.
    #[serde(default)]
    pub adapters: Vec<String>,
    /// Model ids loadable without a path (built-ins).
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub uptime_s: f64,
    pub frames_processed: u64,
    #[serde(default)]
    pub loaded_models: Vec<String>,
    #[serde(default)]
    pub queue_depth: u32,
    #[serde(default)]
    pub gpu_mem_mb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelLoad {
    pub model_id: String,
    pub adapter: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub load_ms: f64,
    #[serde(default)]
    pub adapter: String,
    #[serde(default)]
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameSubmit {
    pub seq: u64,
    pub timestamp_us: u64,
    pub model_id: String,
    pub width: u32,
    pub height: u32,
    /// `shm` only.
    #[serde(default)]
    pub input_slot: Option<u32>,
    #[serde(default)]
    pub output_slot: Option<u32>,
    /// `shm` only: extra inputs by name (e.g. `control`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aux: BTreeMap<String, u32>,
    /// Per-adapter parameters.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameResult {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub output_slot: Option<u32>,
    /// Time the host spent on the frame, for the router's latency meter.
    pub host_ms: f64,
    /// The similarity filter reused the previous output.
    #[serde(default)]
    pub skipped: bool,
    /// Seconds a metered cloud service billed since the previous result
    /// (Decart); when present the spend meter charges these instead of
    /// wall-clock time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCancel {
    pub seq: u64,
}

/// JSON-RPC error codes used by hosts beyond the standard range.
pub mod codes {
    pub const VERSION_MISMATCH: i64 = -32001;
    pub const UNKNOWN_MODEL: i64 = -32002;
    pub const BAD_SLOT: i64 = -32003;
    pub const STALE_FRAME: i64 = -32004;
    pub const UNAUTHORIZED: i64 = -32005;
    pub const BUSY: i64 = -32006;
    pub const MODEL_LOAD_FAILED: i64 = -32007;
    pub const UNSUPPORTED: i64 = -32008;
}
