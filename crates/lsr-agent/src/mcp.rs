//! The MCP server (FR-G1, A15): the agent's typed tools offered to any
//! Model Context Protocol client (Claude Code, other editors and agents).
//!
//! [`handle`] answers one JSON-RPC message (`initialize`, `ping`,
//! `tools/list`, `tools/call`); transports call it from the thread that owns
//! the workspace. Two transports: newline-delimited JSON on stdio
//! ([`read_stdio`]), and HTTP `POST /mcp` with a JSON body on a loopback
//! port ([`serve_http`], which never listens beyond this machine).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

use crate::tools::{Agent, Output, output_text};
use crate::workspace::Workspace;

/// The MCP revision this server speaks.
pub const PROTOCOL: &str = "2025-06-18";

/// Answers one message; `None` for notifications.
pub fn handle(agent: &mut Agent, ws: &mut dyn Workspace, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "lasair", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": agent.tools().iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.schema,
            })).collect::<Vec<_>>()
        })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let out = agent.call(ws, name, &args);
            Ok(match &out {
                Output::Png(png, what) => json!({
                    "content": [
                        {"type": "image", "data": base64(png), "mimeType": "image/png"},
                        {"type": "text", "text": what},
                    ]
                }),
                Output::Error(e) => {
                    json!({"content": [{"type": "text", "text": e}], "isError": true})
                }
                Output::Json(_) => {
                    json!({"content": [{"type": "text", "text": output_text(&out)}]})
                }
            })
        }
        other => Err((-32601, format!("unknown method {other}"))),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

/// What the server tells a client about working with Lasair.
const INSTRUCTIONS: &str = "Lasair is a node-based real-time visual instrument. Build patches with list_ops / get_schema / create_node / connect / set_param. Changes are staged: review_changes shows them; accept_changes (when offered) applies them as one undo step and returns node errors to fix. Wire types must match (IMG→IMG, SIG→SIG, GEO→GEO). Image operators are img.*, channel (audio/control) operators sig.*, points pnt.*, meshes msh.*. Use screenshot_viewer to look at an image node. create_pack turns nodes into a reusable Pack (staged like other edits). During a show, cue turns a performer's words (`darker, slower on the next drop`) into moves of whitelisted parameters, fired on the beat.";

/// A request from a transport, with where to send the answer.
pub type Request = (Value, Sender<Option<Value>>);

/// Reads newline-delimited JSON-RPC from stdin on a thread; each message is
/// sent with a reply channel, and answers are written to stdout in order.
pub fn read_stdio() -> Receiver<Request> {
    let (tx, rx) = channel::<Request>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut out = std::io::stdout();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    let err = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}});
                    let _ = writeln!(out, "{err}");
                    let _ = out.flush();
                    continue;
                }
            };
            let (rtx, rrx) = channel();
            if tx.send((msg, rtx)).is_err() {
                break;
            }
            if let Ok(Some(answer)) = rrx.recv() {
                let _ = writeln!(out, "{answer}");
                let _ = out.flush();
            }
        }
    });
    rx
}

/// Serves `POST /mcp` (JSON-RPC body, JSON answer) on `listener`, which the
/// caller binds to loopback. Each request goes through `tx`.
pub fn serve_http(listener: TcpListener, tx: Sender<Request>) {
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = http_conn(conn, &tx);
            });
        }
    });
}

fn http_conn(conn: TcpStream, tx: &Sender<Request>) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut out = conn;
    loop {
        let mut start = String::new();
        if reader.read_line(&mut start)? == 0 {
            return Ok(());
        }
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            reader.read_line(&mut h)?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; len.min(16 << 20)];
        reader.read_exact(&mut body)?;
        let (status, answer) = if !start.starts_with("POST /mcp") {
            ("404 Not Found", String::new())
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Err(e) => (
                    "400 Bad Request",
                    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}).to_string(),
                ),
                Ok(msg) => {
                    let (rtx, rrx) = channel();
                    let _ = tx.send((msg, rtx));
                    match rrx.recv().ok().flatten() {
                        Some(a) => ("200 OK", a.to_string()),
                        None => ("202 Accepted", String::new()),
                    }
                }
            }
        };
        write!(
            out,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{answer}",
            answer.len()
        )?;
        out.flush()?;
    }
}

/// Standard base64 (for images in tool results).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
