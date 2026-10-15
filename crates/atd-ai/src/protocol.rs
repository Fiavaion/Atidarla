//! AI Host Protocol v1 prototype: message types.
//!
//! Control messages are JSON-RPC 2.0 objects, one per line (`\n`-delimited),
//! over a local stream socket. Frame pixels never travel on the control
//! channel: `frame.submit` names a ring slot (see [`crate::ring`]) and the
//! host answers with the slot holding the result. Sequence IDs and
//! microsecond timestamps let the Output Governor drop or reorder results.
//!
//! | Method            | Params                         | Result              |
//! |-------------------|--------------------------------|---------------------|
//! | `session.open`    | [`SessionOpen`]                | [`SessionInfo`]     |
//! | `host.capabilities` | –                            | [`Capabilities`]    |
//! | `host.health`     | –                              | [`Health`]          |
//! | `model.load`      | [`ModelRef`]                   | [`ModelInfo`]       |
//! | `model.unload`    | [`ModelRef`]                   | `null`              |
//! | `frame.submit`    | [`FrameSubmit`]                | [`FrameResult`]     |
//! | `frame.cancel`    | [`FrameCancel`]                | `null`              |
//! | `session.close`   | –                              | `null`              |

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version negotiated in `session.open`.
pub const PROTOCOL_VERSION: &str = "0.1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOpen {
    pub protocol_version: String,
    pub client: String,
    /// Path of the shared-memory ring the host should map.
    pub ring_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub protocol_version: String,
    pub host_name: String,
    pub host_version: String,
}

/// What a host can run; used by the compute router (FR-A7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// `nvidia`, `amd`, `intel`, `apple`, `cpu`.
    pub vendor: String,
    pub vram_mb: Option<u64>,
    /// e.g. `torch-cuda`, `tensorrt`, `coreml`, `onnxruntime`, `cpu-reference`.
    pub runtimes: Vec<String>,
    /// Frame transports this host accepts: `shm` and, on Windows + NVIDIA, `cuda-ipc`.
    pub transports: Vec<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub uptime_s: f64,
    pub frames_processed: u64,
    pub loaded_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub load_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSubmit {
    pub seq: u64,
    pub timestamp_us: u64,
    pub model_id: String,
    pub input_slot: u32,
    pub output_slot: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameResult {
    pub seq: u64,
    pub output_slot: u32,
    /// Time the host spent on the frame, for the router's latency meter.
    pub host_ms: f64,
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
}
