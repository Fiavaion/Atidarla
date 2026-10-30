//! OSCQuery: advertises a project's parameters (FR-S5).
//!
//! An HTTP server on its own thread answers the OSCQuery proposal's JSON
//! (<https://github.com/Vidvox/OSCQueryProposal>): `GET /` gives the
//! namespace tree (one container per node, one entry per parameter with
//! `FULL_PATH`, `TYPE`, `VALUE`, `RANGE`, `ACCESS`, `DESCRIPTION`),
//! `GET /<node>/<param>` one entry, `?VALUE` (or any attribute) just that
//! attribute, and `GET /?HOST_INFO` the server's name and OSC port. mDNS
//! announces `_oscjson._tcp` (HTTP) and `_osc._udp` (OSC), so controller
//! apps find the project. OSC sent to an advertised address sets the
//! parameter as a live value (see [`crate::mapping::Route`]).
//!
//! The server never touches the engine. It answers from the last list the
//! app published; when a request arrives it asks for a fresh one (see
//! [`OscQueryServer::wants_values`]) and waits up to [`FRESH_WAIT`] for
//! it, so the app builds the list only when somebody is asking.
//! The minimal HTTP/1.1 server is written here (GET only, one request per
//! connection) to avoid a web framework; WebSocket `LISTEN` is not offered.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use atd_core::graph::{Network, NodeId};
use atd_core::registry::{ParamKind, Registry};
use serde_json::{Map, Value, json};

use crate::mapping::{Route, Waker, mappable};

/// How long a request waits for the app to publish current values.
pub const FRESH_WAIT: Duration = Duration::from_millis(100);

/// The mDNS service types.
pub const HTTP_SERVICE: &str = "_oscjson._tcp.local.";
pub const OSC_SERVICE: &str = "_osc._udp.local.";

/// One advertised parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Advertised {
    /// `/<node name>/<param>`.
    pub address: String,
    pub node: NodeId,
    pub param: String,
    pub kind: ParamKind,
    /// The value now (live value, else stored, else default).
    pub value: Value,
    pub description: String,
}

/// An OSC address part: characters OSC reserves (space `#*,/?[]{}`) become `_`.
pub fn address_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || " #*,/?[]{}".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Every parameter of every node in `net`, by node ID then schema order.
pub fn advertise<'a>(
    net: &'a Network,
    registry: &'a Registry,
    live: impl Fn(NodeId, &str) -> Option<&'a Value>,
) -> Vec<Advertised> {
    let mut out = Vec::new();
    for n in net.nodes() {
        let Some(schema) = registry.get(&n.type_id) else {
            continue;
        };
        let node = address_part(&n.name);
        for p in &schema.params {
            let value = live(n.id, &p.name)
                .or_else(|| net.param_value(registry, n.id, &p.name))
                .cloned()
                .unwrap_or(Value::Null);
            out.push(Advertised {
                address: format!("/{node}/{}", address_part(&p.name)),
                node: n.id,
                param: p.name.clone(),
                kind: p.kind.clone(),
                value,
                description: if p.help.is_empty() {
                    p.label.clone()
                } else {
                    format!("{}: {}", p.label, p.help)
                },
            });
        }
    }
    out
}

/// The addresses incoming OSC may set (text and file paths are read-only).
pub fn routes(list: &[Advertised]) -> Vec<Route> {
    list.iter()
        .filter(|a| mappable(&a.kind))
        .map(|a| Route {
            address: a.address.clone(),
            node: a.node,
            param: a.param.clone(),
            kind: a.kind.clone(),
        })
        .collect()
}

/// Changes when nodes are added, removed or renamed (not on value edits):
/// the app republishes routes when it does. Allocates nothing.
pub fn fingerprint(net: &Network) -> u64 {
    let mut h = DefaultHasher::new();
    for n in net.nodes() {
        n.id.hash(&mut h);
        n.name.hash(&mut h);
        n.type_id.hash(&mut h);
    }
    h.finish()
}

fn type_tag(kind: &ParamKind) -> String {
    match kind {
        ParamKind::Float { .. } => "f".into(),
        ParamKind::Int { .. } => "i".into(),
        ParamKind::Toggle => "T".into(),
        ParamKind::Pulse => "I".into(),
        ParamKind::Colour => "ffff".into(),
        ParamKind::Vec { n } => "f".repeat(usize::from(*n)),
        ParamKind::Menu { .. } | ParamKind::String | ParamKind::Path => "s".into(),
    }
}

fn range(kind: &ParamKind) -> Option<Value> {
    let limits = |min: Option<f64>, max: Option<f64>| {
        let mut m = Map::new();
        if let Some(v) = min {
            m.insert("MIN".into(), json!(v));
        }
        if let Some(v) = max {
            m.insert("MAX".into(), json!(v));
        }
        Value::Object(m)
    };
    match kind {
        ParamKind::Float { min, max } if min.is_some() || max.is_some() => {
            Some(json!([limits(*min, *max)]))
        }
        ParamKind::Int { min, max } if min.is_some() || max.is_some() => Some(json!([limits(
            min.map(|v| v as f64),
            max.map(|v| v as f64)
        )])),
        ParamKind::Toggle => Some(json!([{ "VALS": [false, true] }])),
        ParamKind::Menu { options } => Some(json!([{ "VALS": options }])),
        ParamKind::Colour => Some(Value::Array(vec![limits(Some(0.0), Some(1.0)); 4])),
        _ => None,
    }
}

fn value_array(a: &Advertised) -> Option<Value> {
    match (&a.kind, &a.value) {
        (ParamKind::Pulse, _) | (_, Value::Null) => None,
        (_, Value::Array(v)) => Some(Value::Array(v.clone())),
        (_, v) => Some(json!([v])),
    }
}

/// One parameter's entry.
fn entry(a: &Advertised) -> Value {
    let mut m = Map::new();
    m.insert("FULL_PATH".into(), json!(a.address));
    m.insert("TYPE".into(), json!(type_tag(&a.kind)));
    let access = match &a.kind {
        ParamKind::Pulse => 2,
        k if mappable(k) => 3,
        _ => 1,
    };
    m.insert("ACCESS".into(), json!(access));
    if let Some(v) = value_array(a) {
        m.insert("VALUE".into(), v);
    }
    if let Some(r) = range(&a.kind) {
        m.insert("RANGE".into(), r);
    }
    m.insert("DESCRIPTION".into(), json!(a.description));
    Value::Object(m)
}

fn container(path: &str, contents: Map<String, Value>) -> Value {
    json!({ "FULL_PATH": path, "ACCESS": 0, "CONTENTS": contents })
}

/// The JSON for `path` (`/`, `/node` or `/node/param`); `None` if nothing
/// is there.
pub fn tree_json(list: &[Advertised], path: &str) -> Option<Value> {
    let path = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    if let Some(a) = list.iter().find(|a| a.address == path) {
        return Some(entry(a));
    }
    let mut nodes: BTreeMap<&str, Map<String, Value>> = BTreeMap::new();
    for a in list {
        let rest = &a.address[1..];
        let (node, param) = rest.split_once('/').unwrap_or((rest, ""));
        nodes
            .entry(node)
            .or_default()
            .insert(param.to_owned(), entry(a));
    }
    if path == "/" {
        let contents = nodes
            .into_iter()
            .map(|(n, c)| (n.to_owned(), container(&format!("/{n}"), c)))
            .collect();
        let mut root = container("/", contents);
        root["DESCRIPTION"] = json!("Atidarla parameters");
        return Some(root);
    }
    let node = path.strip_prefix('/')?;
    nodes.remove(node).map(|c| container(path, c))
}

/// `?HOST_INFO`: name, OSC port and the attributes this server answers.
pub fn host_info(name: &str, osc_port: u16) -> Value {
    json!({
        "NAME": name,
        "OSC_PORT": osc_port,
        "OSC_TRANSPORT": "UDP",
        "EXTENSIONS": {
            "ACCESS": true,
            "VALUE": true,
            "RANGE": true,
            "DESCRIPTION": true,
            "TYPE": true,
            "FULL_PATH": true,
            "CONTENTS": true,
            "TAGS": false,
            "CLIPMODE": false,
            "UNIT": false,
            "CRITICAL": false,
            "LISTEN": false,
            "PATH_CHANGED": false,
        }
    })
}

/// The response to `GET <target>`: status and JSON body.
pub fn respond(list: &[Advertised], name: &str, osc_port: u16, target: &str) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = if path.is_empty() { "/" } else { path };
    if query == "HOST_INFO" {
        return (200, host_info(name, osc_port));
    }
    let Some(node) = tree_json(list, path) else {
        return (404, json!({ "error": format!("no OSC address {path}") }));
    };
    if query.is_empty() {
        return (200, node);
    }
    match node.get(query) {
        Some(v) => (200, json!({ query: v })),
        // The proposal: an attribute the node lacks is "no content".
        None => (204, Value::Null),
    }
}

#[derive(Debug, Default)]
struct Snapshot {
    /// Shared so a request builds its response after letting go of the
    /// lock (the app publishes under it).
    list: Arc<Vec<Advertised>>,
    generation: u64,
}

#[derive(Debug, Default)]
struct Shared {
    snapshot: Mutex<Snapshot>,
    published: Condvar,
    want: AtomicBool,
    stop: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

/// How to run the server.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Shown to clients (`HOST_INFO`, the mDNS instance name).
    pub name: String,
    /// HTTP port (0 picks one; see [`OscQueryServer::port`]).
    pub http_port: u16,
    /// Where OSC to the advertised addresses goes.
    pub osc_port: u16,
    /// Listen on this computer only (tests); otherwise every interface.
    pub loopback: bool,
    /// Announce over mDNS.
    pub mdns: bool,
}

/// The HTTP server and mDNS announcement; both stop when dropped.
#[derive(Debug)]
pub struct OscQueryServer {
    shared: Arc<Shared>,
    port: u16,
    thread: Option<JoinHandle<()>>,
    mdns: Option<Mdns>,
    mdns_error: Option<String>,
}

impl OscQueryServer {
    pub fn start(opts: ServerOptions) -> io::Result<Self> {
        let addr: IpAddr = if opts.loopback {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        let listener = TcpListener::bind((addr, opts.http_port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared::default());
        let sh = Arc::clone(&shared);
        let (name, osc_port) = (opts.name.clone(), opts.osc_port);
        let thread = std::thread::Builder::new()
            .name(format!("atd-oscquery-{port}"))
            .spawn(move || {
                while !sh.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(e) = serve(stream, &sh, &name, osc_port) {
                                log::debug!("OSCQuery request: {e}");
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => {
                            log::warn!("OSCQuery accept failed: {e}");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })?;
        let mut server = Self {
            shared,
            port,
            thread: Some(thread),
            mdns: None,
            mdns_error: None,
        };
        if opts.mdns {
            match announce(&opts.name, port, opts.osc_port) {
                Ok(d) => server.mdns = Some(d),
                Err(e) => {
                    log::warn!("OSCQuery mDNS: {e}");
                    server.mdns_error = Some(e);
                }
            }
        }
        Ok(server)
    }

    /// The HTTP port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Why mDNS is not announcing, if it is not.
    pub fn mdns_error(&self) -> Option<&str> {
        self.mdns_error.as_deref()
    }

    /// Whether mDNS is announcing.
    pub fn announcing(&self) -> bool {
        self.mdns.is_some()
    }

    /// A request is waiting for current values: publish them (cheap to
    /// call every frame).
    pub fn wants_values(&self) -> bool {
        self.shared.want.load(Ordering::Relaxed)
    }

    /// Sets what to call when a request wants values (so an idle UI runs
    /// a frame and publishes).
    pub fn set_waker(&self, waker: Option<Waker>) {
        *self.shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = waker;
    }

    /// Replaces the advertised parameters.
    pub fn publish(&self, list: Vec<Advertised>) {
        let list = Arc::new(list);
        let mut s = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        s.list = list;
        s.generation += 1;
        self.shared.want.store(false, Ordering::Relaxed);
        self.shared.published.notify_all();
    }
}

impl Drop for OscQueryServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(d) = self.mdns.take() {
            let _ = d.0.shutdown();
        }
    }
}

/// The mDNS daemon (its own thread), announcing until shut down.
struct Mdns(mdns_sd::ServiceDaemon);

impl std::fmt::Debug for Mdns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mdns")
    }
}

fn announce(name: &str, http_port: u16, osc_port: u16) -> Result<Mdns, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
    let host = format!("atidarla-{}.local.", std::process::id());
    for (ty, port) in [(HTTP_SERVICE, http_port), (OSC_SERVICE, osc_port)] {
        let info = mdns_sd::ServiceInfo::new(ty, name, &host, (), port, None)
            .map_err(|e| e.to_string())?
            .enable_addr_auto();
        daemon.register(info).map_err(|e| e.to_string())?;
    }
    Ok(Mdns(daemon))
}

fn serve(mut stream: TcpStream, sh: &Shared, name: &str, osc_port: u16) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk)?;
        if n == 0 || buf.len() > 16 * 1024 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let mut words = head.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or("/"));
    let (status, body) = if method != "GET" {
        (405, json!({ "error": "only GET" }))
    } else {
        let target = percent_decode(target);
        // Ask the app for current values and wait briefly for them.
        let mut s = sh.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        let seen = s.generation;
        sh.want.store(true, Ordering::Relaxed);
        let waker = sh.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = waker {
            (w.0)();
        }
        let until = Instant::now() + FRESH_WAIT;
        while s.generation == seen {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            s = sh
                .published
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let list = Arc::clone(&s.list);
        drop(s);
        respond(&list, name, osc_port, &target)
    };
    let text = match status {
        200 => "OK",
        204 => "No Content",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    let body = if status == 204 {
        String::new()
    } else {
        body.to_string()
    };
    write!(
        stream,
        "HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests;
