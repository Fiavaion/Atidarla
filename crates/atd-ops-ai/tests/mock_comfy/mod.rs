//! A mock ComfyUI server for tests, on 127.0.0.1 (no ComfyUI code: written
//! from the API's documented shapes). It speaks the parts of the HTTP and
//! WebSocket API Atidarla uses and really runs `LoadImage`, `ImageInvert`,
//! `TestTint` (passes its image through) and `SaveImage`/`PreviewImage` on
//! the pixels, so a round trip can be checked byte for byte.
//!
//! Shared by `atd-ops-ai`'s tests and `atd-cli`'s render test
//! (`#[path = ...] mod mock_comfy;`).

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// How the mock reports progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `executing` + `progress` (older servers).
    Legacy,
    /// `progress_state` (newer servers).
    State,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub form: Form,
    /// Time each node takes.
    pub node_delay: Duration,
    /// Node class that fails while running.
    pub fail_class: Option<String>,
    /// Whether to send WebSocket events at all.
    pub events: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            form: Form::Legacy,
            node_delay: Duration::ZERO,
            fail_class: None,
            events: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct State {
    /// Uploaded files by `subfolder/name`.
    pub uploads: HashMap<String, Vec<u8>>,
    pub outputs: HashMap<String, Vec<u8>>,
    pub history: HashMap<String, Value>,
    /// Every prompt queued, in order.
    pub prompts: Vec<Value>,
    /// Requests as `METHOD /path`.
    pub requests: Vec<String>,
    pub running: usize,
    pub max_running: usize,
    clients: HashMap<String, Sender<String>>,
    next: u64,
}

/// The node classes the mock has (its `/object_info`).
pub fn object_info() -> Value {
    json!({
        "LoadImage": {
            "input": {"required": {"image": [["example.png", "other.png"], {"image_upload": true}]}},
            "input_order": {"required": ["image"]},
            "output": ["IMAGE", "MASK"], "output_is_list": [false, false], "output_name": ["IMAGE", "MASK"],
            "name": "LoadImage", "display_name": "Load Image", "description": "", "category": "image",
            "output_node": false
        },
        "ImageInvert": {
            "input": {"required": {"image": ["IMAGE"]}},
            "input_order": {"required": ["image"]},
            "output": ["IMAGE"], "output_is_list": [false], "output_name": ["IMAGE"],
            "name": "ImageInvert", "display_name": "Invert Image", "description": "", "category": "image",
            "output_node": false
        },
        "SaveImage": {
            "input": {
                "required": {"images": ["IMAGE", {"tooltip": "The images to save."}],
                             "filename_prefix": ["STRING", {"default": "ComfyUI"}]},
                "hidden": {"prompt": "PROMPT", "extra_pnginfo": "EXTRA_PNGINFO"}
            },
            "input_order": {"required": ["images", "filename_prefix"], "hidden": ["prompt", "extra_pnginfo"]},
            "output": [], "output_is_list": [], "output_name": [],
            "name": "SaveImage", "display_name": "Save Image", "description": "", "category": "image",
            "output_node": true
        },
        "PreviewImage": {
            "input": {"required": {"images": ["IMAGE"]}, "hidden": {"prompt": "PROMPT"}},
            "input_order": {"required": ["images"]},
            "output": [], "name": "PreviewImage", "display_name": "Preview Image", "category": "image",
            "output_node": true
        },
        "TestTint": {
            "input": {"required": {
                "image": ["IMAGE"],
                "seed": ["INT", {"default": 0, "min": 0, "max": 18446744073709551615u64, "control_after_generate": true}],
                "strength": ["FLOAT", {"default": 0.5, "min": 0.0, "max": 1.0, "step": 0.01}],
                "mode": [["add", "multiply"], {}],
                "blend": ["COMBO", {"options": ["soft", "hard"], "default": "soft"}],
                "note": ["STRING", {"multiline": true, "default": ""}],
                "enabled": ["BOOLEAN", {"default": true}]
            }},
            "input_order": {"required": ["image", "seed", "strength", "mode", "blend", "note", "enabled"]},
            "output": ["IMAGE"], "name": "TestTint", "display_name": "Test Tint", "category": "test",
            "output_node": false
        }
    })
}

/// A running mock server; stops accepting when the test process ends.
#[derive(Debug, Clone)]
pub struct MockComfy {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<State>>,
    pub config: Arc<Mutex<Config>>,
}

impl MockComfy {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = Self {
            addr,
            state: Arc::default(),
            config: Arc::default(),
        };
        let m = mock.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let m = m.clone();
                std::thread::spawn(move || m.serve(stream));
            }
        });
        mock
    }

    /// `127.0.0.1:<port>`.
    pub fn server(&self) -> String {
        self.addr.to_string()
    }

    pub fn configure(&self, f: impl FnOnce(&mut Config)) {
        f(&mut self.config.lock().unwrap());
    }

    pub fn prompts(&self) -> Vec<Value> {
        self.state.lock().unwrap().prompts.clone()
    }

    fn serve(&self, mut stream: TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        // Look at the request head without consuming it, so a WebSocket
        // handshake can read it again.
        let mut buf = vec![0u8; 16 * 1024];
        let head_len = loop {
            let Ok(n) = stream.peek(&mut buf) else { return };
            if n == 0 {
                return;
            }
            if let Some(i) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if n == buf.len() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
        let mut first = head.lines().next().unwrap_or("").split(' ');
        let method = first.next().unwrap_or("").to_owned();
        let target = first.next().unwrap_or("").to_owned();
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let (path, query) = (path.to_owned(), query.to_owned());
        self.state
            .lock()
            .unwrap()
            .requests
            .push(format!("{method} {path}"));
        if path == "/ws" {
            self.websocket(stream, &query);
            return;
        }
        let mut head_bytes = vec![0u8; head_len];
        if stream.read_exact(&mut head_bytes).is_err() {
            return;
        }
        let header = |name: &str| {
            head.lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
                .map(|(_, v)| v.trim().to_owned())
        };
        let len: usize = header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        let (status, kind, out) = match (method.as_str(), path.as_str()) {
            ("GET", "/object_info") => ok_json(&object_info()),
            ("POST", "/upload/image") => {
                self.upload(&header("content-type").unwrap_or_default(), &body)
            }
            ("POST", "/prompt") => self.prompt(&body),
            ("GET", "/view") => self.view(&query),
            ("GET", p) if p.starts_with("/history/") => {
                let id = &p["/history/".len()..];
                let s = self.state.lock().unwrap();
                ok_json(&match s.history.get(id) {
                    Some(h) => json!({ id: h }),
                    None => json!({}),
                })
            }
            _ => (404, "text/plain", b"not found".to_vec()),
        };
        let reason = if status == 200 { "OK" } else { "Error" };
        let _ = write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            out.len()
        );
        let _ = stream.write_all(&out);
    }

    fn websocket(&self, stream: TcpStream, query: &str) {
        let Ok(mut ws) = tungstenite::accept(stream) else {
            return;
        };
        let client = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("clientId="))
            .unwrap_or("")
            .to_owned();
        let (tx, rx) = channel();
        self.state
            .lock()
            .unwrap()
            .clients
            .insert(client.clone(), tx);
        let hello = json!({"type": "status", "data": {"status": {"exec_info": {"queue_remaining": 0}}, "sid": client}});
        if ws
            .send(tungstenite::Message::text(hello.to_string()))
            .is_err()
        {
            return;
        }
        // A preview frame, which clients ignore.
        let _ = ws.send(tungstenite::Message::binary(vec![0u8, 0, 0, 1, 0xFF]));
        for text in rx {
            if ws.send(tungstenite::Message::text(text)).is_err() {
                return;
            }
        }
    }

    fn upload(&self, content_type: &str, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
        let Some(boundary) = content_type.split("boundary=").nth(1) else {
            return (400, "text/plain", b"no boundary".to_vec());
        };
        let delim = format!("--{boundary}");
        let mut fields: BTreeMap<String, (Option<String>, Vec<u8>)> = BTreeMap::new();
        for part in split(body, delim.as_bytes()) {
            let Some(i) = part.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&part[..i]);
            let mut data = &part[i + 4..];
            if data.ends_with(b"\r\n") {
                data = &data[..data.len() - 2];
            }
            let attr = |k: &str| {
                head.split(&format!("{k}=\""))
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .map(str::to_owned)
            };
            if let Some(name) = attr(" name").or_else(|| attr("name")) {
                fields.insert(name, (attr("filename"), data.to_vec()));
            }
        }
        let Some((Some(filename), bytes)) = fields.get("image").cloned() else {
            return (400, "text/plain", b"no image".to_vec());
        };
        let sub = fields
            .get("subfolder")
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();
        let key = if sub.is_empty() {
            filename.clone()
        } else {
            format!("{sub}/{filename}")
        };
        self.state.lock().unwrap().uploads.insert(key, bytes);
        ok_json(&json!({"name": filename, "subfolder": sub, "type": "input"}))
    }

    fn prompt(&self, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            return (400, "text/plain", b"bad json".to_vec());
        };
        let prompt = v["prompt"].clone();
        let client = v["client_id"].as_str().unwrap_or("").to_owned();
        let info = object_info();
        let mut node_errors = serde_json::Map::new();
        let state = self.state.lock().unwrap();
        for (id, node) in prompt.as_object().into_iter().flatten() {
            let class = node["class_type"].as_str().unwrap_or("");
            let error = |message: &str, details: String| {
                json!({"errors": [{"type": "custom_validation_failed", "message": message, "details": details, "extra_info": {}}],
                       "dependent_outputs": [], "class_type": class})
            };
            if info.get(class).is_none() {
                node_errors.insert(id.clone(), error("Node class not found", class.to_owned()));
            } else if class == "LoadImage" {
                let image = node["inputs"]["image"].as_str().unwrap_or("");
                if image != "example.png" && !state.uploads.contains_key(image) {
                    node_errors.insert(
                        id.clone(),
                        error("Invalid image file", format!("image: {image}")),
                    );
                }
            } else if class == "TestTint"
                && node["inputs"]["strength"].as_f64().is_some_and(|s| s > 1.0)
            {
                node_errors.insert(
                    id.clone(),
                    error("Value bigger than max of 1.0", "strength".into()),
                );
            }
        }
        drop(state);
        if !node_errors.is_empty() {
            let body = json!({
                "error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation", "details": "", "extra_info": {}},
                "node_errors": node_errors
            });
            return (400, "application/json", body.to_string().into_bytes());
        }
        let (id, number) = {
            let mut s = self.state.lock().unwrap();
            s.next += 1;
            s.prompts.push(prompt.clone());
            s.running += 1;
            s.max_running = s.max_running.max(s.running);
            (format!("prompt-{}", s.next), s.next)
        };
        let m = self.clone();
        let pid = id.clone();
        std::thread::spawn(move || m.execute(&pid, &prompt, &client));
        ok_json(&json!({"prompt_id": id, "number": number, "node_errors": {}}))
    }

    fn send(&self, client: &str, kind: &str, data: Value) {
        if !self.config.lock().unwrap().events {
            return;
        }
        let tx = self.state.lock().unwrap().clients.get(client).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(json!({"type": kind, "data": data}).to_string());
        }
    }

    fn execute(&self, id: &str, prompt: &Value, client: &str) {
        let config = self.config.lock().unwrap().clone();
        self.send(
            client,
            "execution_start",
            json!({"prompt_id": id, "timestamp": 0}),
        );
        self.send(
            client,
            "execution_cached",
            json!({"nodes": [], "prompt_id": id, "timestamp": 0}),
        );
        let nodes = prompt.as_object().cloned().unwrap_or_default();
        let order = topo(&nodes);
        let mut images: HashMap<String, Img> = HashMap::new();
        let mut outputs = serde_json::Map::new();
        let mut finished: Vec<String> = Vec::new();
        let mut failure = None;
        for nid in &order {
            let node = &nodes[nid];
            let class = node["class_type"].as_str().unwrap_or("");
            match config.form {
                Form::Legacy => {
                    self.send(
                        client,
                        "executing",
                        json!({"node": nid, "display_node": nid, "prompt_id": id}),
                    );
                    self.send(
                        client,
                        "progress",
                        json!({"value": 1, "max": 2, "prompt_id": id, "node": nid}),
                    );
                }
                Form::State => {
                    let mut states = serde_json::Map::new();
                    for f in &finished {
                        states.insert(f.clone(), json!({"value": 1, "max": 1, "state": "finished", "node_id": f, "prompt_id": id}));
                    }
                    states.insert(nid.clone(), json!({"value": 1, "max": 2, "state": "running", "node_id": nid, "prompt_id": id}));
                    self.send(
                        client,
                        "progress_state",
                        json!({"prompt_id": id, "nodes": states}),
                    );
                }
            }
            std::thread::sleep(config.node_delay);
            if config.fail_class.as_deref() == Some(class) {
                failure = Some(json!({"prompt_id": id, "node_id": nid, "node_type": class,
                    "executed": finished, "exception_message": "mock failure\n", "exception_type": "RuntimeError",
                    "traceback": [], "current_inputs": {}, "current_outputs": {}}));
                break;
            }
            let input = |name: &str| -> Option<Img> {
                let link = node["inputs"][name].as_array()?;
                images.get(link[0].as_str()?).cloned()
            };
            match class {
                "LoadImage" => {
                    let name = node["inputs"]["image"].as_str().unwrap_or("");
                    let bytes = self.state.lock().unwrap().uploads.get(name).cloned();
                    let img = match bytes {
                        Some(b) => decode(&b),
                        None => Img {
                            w: 2,
                            h: 2,
                            rgb: vec![128; 12],
                        },
                    };
                    images.insert(nid.clone(), img);
                }
                "ImageInvert" => {
                    let mut img = input("image").expect("linked image");
                    for v in &mut img.rgb {
                        *v = 255 - *v;
                    }
                    images.insert(nid.clone(), img);
                }
                "TestTint" => {
                    let img = input("image").expect("linked image");
                    images.insert(nid.clone(), img);
                }
                "SaveImage" | "PreviewImage" => {
                    let img = input("images").expect("linked images");
                    let (prefix, kind) = if class == "SaveImage" {
                        (
                            node["inputs"]["filename_prefix"]
                                .as_str()
                                .unwrap_or("ComfyUI")
                                .to_owned(),
                            "output",
                        )
                    } else {
                        ("ComfyUI_temp".to_owned(), "temp")
                    };
                    let file = format!("{prefix}_{id}_{nid}.png");
                    self.state
                        .lock()
                        .unwrap()
                        .outputs
                        .insert(file.clone(), encode(&img));
                    let ui = json!({"images": [{"filename": file, "subfolder": "", "type": kind}]});
                    self.send(
                        client,
                        "executed",
                        json!({"node": nid, "display_node": nid, "output": ui, "prompt_id": id}),
                    );
                    outputs.insert(nid.clone(), ui);
                }
                _ => {}
            }
            finished.push(nid.clone());
        }
        let entry = match &failure {
            Some(f) => {
                json!({"prompt": [], "outputs": {}, "status": {"status_str": "error", "completed": false,
                              "messages": [["execution_start", {"prompt_id": id}], ["execution_error", f]]}, "meta": {}})
            }
            None => {
                json!({"prompt": [], "outputs": outputs, "status": {"status_str": "success", "completed": true,
                           "messages": [["execution_start", {"prompt_id": id}], ["execution_success", {"prompt_id": id}]]}, "meta": {}})
            }
        };
        {
            let mut s = self.state.lock().unwrap();
            s.running -= 1;
            s.history.insert(id.to_owned(), entry);
        }
        match failure {
            Some(f) => self.send(client, "execution_error", f),
            None => self.send(
                client,
                "execution_success",
                json!({"prompt_id": id, "timestamp": 0}),
            ),
        }
        self.send(client, "executing", json!({"node": null, "prompt_id": id}));
    }

    fn view(&self, query: &str) -> (u16, &'static str, Vec<u8>) {
        let file = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("filename="))
            .unwrap_or("");
        match self.state.lock().unwrap().outputs.get(file) {
            Some(b) => (200, "image/png", b.clone()),
            None => (404, "text/plain", b"no such file".to_vec()),
        }
    }
}

fn ok_json(v: &Value) -> (u16, &'static str, Vec<u8>) {
    (200, "application/json", v.to_string().into_bytes())
}

fn split<'a>(body: &'a [u8], delim: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.windows(delim.len()).position(|w| w == delim) {
        parts.push(&rest[..i]);
        rest = &rest[i + delim.len()..];
    }
    parts.push(rest);
    parts
}

/// Nodes in an order where links come first.
fn topo(nodes: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut ids: Vec<&String> = nodes.keys().collect();
    ids.sort_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX));
    while order.len() < ids.len() {
        let before = order.len();
        for id in &ids {
            if order.contains(id) {
                continue;
            }
            let ready = nodes[*id]["inputs"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, v)| v.as_array().and_then(|a| a.first()?.as_str()))
                .all(|src| order.iter().any(|o| o == src));
            if ready {
                order.push((*id).clone());
            }
        }
        assert!(
            order.len() > before,
            "the mock cannot order a cyclic prompt"
        );
    }
    order
}

/// An RGB image, as ComfyUI's IMAGE (no alpha: LoadImage splits it off).
#[derive(Debug, Clone)]
struct Img {
    w: u32,
    h: u32,
    rgb: Vec<u8>,
}

fn decode(bytes: &[u8]) -> Img {
    let mut d = png::Decoder::new(std::io::Cursor::new(bytes));
    d.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = d.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "uploads are RGBA PNGs"
    );
    let rgb = buf[..(info.width * info.height * 4) as usize]
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    Img {
        w: info.width,
        h: info.height,
        rgb,
    }
}

fn encode(img: &Img) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, img.w, img.h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .unwrap()
        .write_image_data(&img.rgb)
        .unwrap();
    out
}
