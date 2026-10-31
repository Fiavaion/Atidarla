//! Web Client (HTTP requests through the egress gate) and Web Server (a
//! small HTTP server, on this machine by default). Requests run and
//! connections are served on worker threads; the cook hands work over and
//! picks results up without waiting (CLAUDE.md rule 1).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atd_ai::models::{DownloadOptions, gated_agent};
use atd_core::dat::{Dat, Table};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{DatCtx, DatOp};

use crate::net::{Mailbox, lock, split_url};
use crate::{dat_in, delim, op_schema};

/// Longest response body Web Client reads.
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;
/// Longest request (headers, then body) Web Server reads.
const MAX_HEADERS: usize = 64 * 1024;
const MAX_BODY: usize = 1024 * 1024;

pub fn client_schema() -> OperatorSchema {
    let mut s = op_schema(
        "web_client",
        "Web Client",
        "Sends an HTTP or HTTPS request on a worker thread and outputs the response body as text (feed JSON to JSON). Requests go on the Fetch pulse, when the URL, method, headers or body change (On Change), and every Interval seconds. Every connection goes through Sovereign mode's egress gate: this machine and the LAN are always reachable, the internet only with Sovereign mode off. The status line shows the HTTP status, size and time; a status of 400 or more is a node error (the body is still output).",
        vec![
            ParamSchema::string("url", "URL", "").help(
                "`http://` or `https://` address, for example `http://127.0.0.1:9980/` (a Web Server on this machine).",
            ),
            ParamSchema::menu("method", "Method", &["GET", "POST", "PUT", "DELETE"], "GET")
                .help("The HTTP method. POST and PUT send the body."),
            ParamSchema::string("headers", "Headers", "")
                .multiline()
                .help("Extra request headers, one `Name: value` per line."),
            ParamSchema::string("body", "Body", "")
                .multiline()
                .help("The request body when the `body` input is not connected."),
            ParamSchema::pulse("fetch", "Fetch").help("Sends the request now."),
            ParamSchema::toggle("on_change", "On Change", true).help(
                "Sends whenever the URL, method, headers or body change (and once when the node starts).",
            ),
            ParamSchema::float("interval_s", "Interval", 0.0, Some(0.0), Some(3600.0))
                .help("Sends again every this many seconds; 0: never on its own."),
            ParamSchema::float("timeout_s", "Timeout", 10.0, Some(0.5), Some(600.0))
                .help("Seconds a request may take before it fails."),
        ],
        vec![dat_in("body", true)],
        40,
    );
    // Polls its worker and its interval every frame.
    s.time_dependent = true;
    s
}

pub fn server_schema() -> OperatorSchema {
    let mut s = op_schema(
        "web_server",
        "Web Server",
        "Serves its `content` input over HTTP on a port of this machine (`http://127.0.0.1:<port>/`, any path), and outputs the requests it received as a table (`method`, `path`, `body`; newest last), so a phone or another program can send text into the show with a POST. Listens on loopback only unless LAN is on. Connections are handled on a worker thread; requests over 1 MB are refused.",
        vec![
            ParamSchema::int("port", "Port", 9980, Some(1), Some(65535))
                .help("The TCP port to listen on."),
            ParamSchema::toggle("lan", "LAN", false).help(
                "Also accepts connections from other machines (listens on every interface). Off: this machine only.",
            ),
            ParamSchema::menu("content_type", "Content Type", &["auto", "text", "json", "csv", "html"], "auto").help(
                "How the content is sent: `auto` sends text as plain text and a table as TSV; `json` and `html` send text with that type; `csv` sends a table as CSV.",
            ),
            ParamSchema::int("keep", "Keep", 20, Some(1), Some(1000))
                .help("Requests kept in the output table."),
        ],
        vec![dat_in("content", true)],
        40,
    );
    s.time_dependent = true;
    s.sink = true;
    s
}

#[derive(Debug)]
struct Request {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout: Duration,
}

#[derive(Debug)]
struct Response {
    status: u16,
    body: String,
    took: Duration,
}

fn parse_headers(text: &str) -> Result<Vec<(String, String)>, String> {
    atd_core::dat::lines(text)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .filter(|(k, _)| !k.is_empty())
                .ok_or_else(|| format!("header `{l}` is not `Name: value`"))
        })
        .collect()
}

fn run(
    agent: &ureq::Agent,
    refused: &Mutex<Option<String>>,
    r: &Request,
) -> Result<Response, String> {
    let start = Instant::now();
    let mut req = ureq::http::Request::builder()
        .method(r.method.as_str())
        .uri(r.url.as_str());
    for (k, v) in &r.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let req = req.body(r.body.clone()).map_err(|e| e.to_string())?;
    let req = agent
        .configure_request(req)
        .timeout_global(Some(r.timeout))
        .build();
    let resp = agent.run(req).map_err(|e| match lock(refused).take() {
        Some(host) => format!(
            "Sovereign mode blocks {host}: it is on the internet, not this machine or the LAN"
        ),
        None => format!("{}: {e}", r.url),
    })?;
    let status = resp.status().as_u16();
    let body = resp
        .into_body()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_string()
        .map_err(|e| format!("{}: {e}", r.url))?;
    Ok(Response {
        status,
        body,
        took: start.elapsed(),
    })
}

type ClientBox = Mailbox<Request, Result<Response, String>>;

struct Worker {
    mailbox: Arc<ClientBox>,
}

impl Worker {
    fn start() -> Self {
        let mailbox: Arc<ClientBox> = Arc::default();
        let m = mailbox.clone();
        std::thread::Builder::new()
            .name("atd-web-client".into())
            .spawn(move || {
                let refused = Arc::new(Mutex::new(None));
                let agent = gated_agent(&DownloadOptions::default(), refused.clone());
                while let Some(job) = m.next() {
                    let r = run(&agent, &refused, &job);
                    m.finish(r);
                }
            })
            .expect("spawning a thread");
        Self { mailbox }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.mailbox.stop();
    }
}

/// What the last request was made from (On Change compares with it).
#[derive(Debug, Default)]
struct Sent {
    url: String,
    method: String,
    headers: String,
    body_param: String,
    body_input: Option<Dat>,
}

#[derive(Default)]
struct ClientOp {
    worker: Option<Worker>,
    sent: Option<Sent>,
    /// A request is due but the worker was busy.
    due: bool,
    last_pulse: bool,
    last_fetch: Option<f64>,
    seen: u64,
    error: Option<String>,
    status: String,
}

impl std::fmt::Debug for ClientOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientOp").finish_non_exhaustive()
    }
}

impl ClientOp {
    fn changed(&self, p: &atd_gpu::engine::Params<'_>, body: Option<&Dat>) -> bool {
        let Some(s) = &self.sent else {
            return true;
        };
        s.url != p.str("url")
            || s.method != p.str("method")
            || s.headers != p.str("headers")
            || match body {
                Some(b) => s.body_input.as_ref().is_none_or(|old| !Dat::ptr_eq(old, b)),
                None => s.body_input.is_some() || s.body_param != p.str("body"),
            }
    }
}

impl DatOp for ClientOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A live input offline (docs/render.md): no requests.
            return Ok(());
        }
        let pulse = p.bool("fetch");
        let mut due = pulse && !self.last_pulse;
        self.last_pulse = pulse;
        let body = ctx.inputs.get(0);
        let url = p.str("url").trim();
        due |= p.bool("on_change") && self.changed(&p, body);
        let interval = f64::from(p.f32("interval_s"));
        due |= interval > 0.0 && self.last_fetch.is_none_or(|t| ctx.time - t >= interval);
        self.due |= due;
        if self.due && url.is_empty() {
            self.due = false;
            self.status.clear();
        }
        if self.due {
            let request = (|| {
                split_url(url, |s| match s {
                    "http" => Some(80),
                    "https" => Some(443),
                    _ => None,
                })?;
                Ok::<_, String>(Request {
                    url: url.to_owned(),
                    method: p.str("method").to_owned(),
                    headers: parse_headers(p.str("headers"))?,
                    body: match body {
                        Some(b) => b.as_text().as_bytes().to_vec(),
                        None => p.str("body").as_bytes().to_vec(),
                    },
                    timeout: Duration::from_secs_f32(p.f32("timeout_s").clamp(0.5, 600.0)),
                })
            })();
            // Whatever happens, this is what was asked for.
            self.sent = Some(Sent {
                url: p.str("url").to_owned(),
                method: p.str("method").to_owned(),
                headers: p.str("headers").to_owned(),
                body_param: p.str("body").to_owned(),
                body_input: body.cloned(),
            });
            match request {
                Err(e) => {
                    self.due = false;
                    self.error = Some(e);
                }
                Ok(r) => {
                    let worker = self.worker.get_or_insert_with(Worker::start);
                    if worker.mailbox.offer(r).is_ok() {
                        self.due = false;
                        self.last_fetch = Some(ctx.time);
                        self.status = "sending…".into();
                    }
                }
            }
        }
        if let Some(w) = &self.worker
            && let Ok(mut s) = w.mailbox.state.try_lock()
            && s.seq != self.seen
        {
            self.seen = s.seq;
            match s.result.take() {
                Some(Ok(r)) => {
                    self.status = format!(
                        "{} · {} bytes · {} ms",
                        r.status,
                        r.body.len(),
                        r.took.as_millis()
                    );
                    self.error = (r.status >= 400).then(|| format!("HTTP {}", r.status));
                    if !(ctx.output.is_text() && ctx.output.as_text() == r.body) {
                        *ctx.output = Dat::text(r.body);
                    }
                }
                Some(Err(e)) => {
                    self.status.clear();
                    self.error = Some(e);
                }
                None => {}
            }
        }
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn create_client() -> Box<dyn DatOp> {
    Box::<ClientOp>::default()
}

/// What the server shares with its worker.
#[derive(Debug, Default)]
struct ServerState {
    content: Dat,
    content_type: String,
    requests: VecDeque<[String; 3]>,
    keep: usize,
    /// Bumped with every request.
    seq: u64,
}

struct Server {
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Reads one request: method, path, body.
fn read_request(stream: &TcpStream) -> Result<(String, String, String), String> {
    let mut reader = BufReader::new(stream.take((MAX_HEADERS + MAX_BODY) as u64));
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("empty request")?.to_owned();
    let path = parts.next().ok_or("no path")?.to_owned();
    let mut length = 0usize;
    let mut header_bytes = line.len();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        header_bytes += n;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if header_bytes > MAX_HEADERS {
            return Err("headers too long".into());
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().map_err(|_| "bad Content-Length")?;
        }
    }
    if length > MAX_BODY {
        return Err("body too large".into());
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok((method, path, String::from_utf8_lossy(&body).into_owned()))
}

fn respond(mut stream: &TcpStream, status: &str, content_type: &str, body: &[u8], head: bool) {
    let header = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head {
        let _ = stream.write_all(body);
    }
}

fn serve_one(stream: &TcpStream, state: &Mutex<ServerState>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let (method, path, body) = match read_request(stream) {
        Ok(r) => r,
        Err(e) => {
            respond(stream, "400 Bad Request", "text/plain", e.as_bytes(), false);
            return;
        }
    };
    let (content, content_type) = {
        let mut s = lock(state);
        s.requests.push_back([method.clone(), path, body]);
        while s.requests.len() > s.keep.max(1) {
            s.requests.pop_front();
        }
        s.seq += 1;
        (s.content.clone(), s.content_type.clone())
    };
    let bytes = match content_type.as_str() {
        "csv" => delim::write(&content.as_table(), "comma").into_bytes(),
        "json" | "html" => content.as_text().into_owned().into_bytes(),
        _ if content.is_text() => content.as_text().into_owned().into_bytes(),
        _ => delim::write(&content.as_table(), "tab").into_bytes(),
    };
    let mime = match content_type.as_str() {
        "json" => "application/json",
        "html" => "text/html; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        _ if content.is_text() => "text/plain; charset=utf-8",
        _ => "text/tab-separated-values; charset=utf-8",
    };
    respond(stream, "200 OK", mime, &bytes, method == "HEAD");
}

impl Server {
    fn start(port: u16, lan: bool) -> Result<Self, String> {
        let ip = if lan {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        // Binding is a quick system call, not a wait.
        let listener = TcpListener::bind((ip, port))
            .map_err(|e| format!("cannot listen on {ip}:{port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let state = Arc::new(Mutex::new(ServerState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (state.clone(), stop.clone());
        std::thread::Builder::new()
            .name("atd-web-server".into())
            .spawn(move || {
                while !st.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => serve_one(&stream, &s),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            })
            .expect("spawning a thread");
        Ok(Self { state, stop, addr })
    }
}

#[derive(Default)]
struct ServerOp {
    server: Option<Server>,
    /// Port and LAN the server was started with (or failed to start with).
    bound: Option<(i64, bool)>,
    error: Option<String>,
    content: Option<Dat>,
    content_type: String,
    keep: i64,
    /// The request count the output table shows (`None`: not built yet).
    seen: Option<u64>,
    status: String,
}

impl std::fmt::Debug for ServerOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerOp")
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

impl DatOp for ServerOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A sink offline (docs/render.md): nothing is served.
            return Ok(());
        }
        let want = (p.i64("port").clamp(1, 65535), p.bool("lan"));
        if self.bound != Some(want) {
            self.bound = Some(want);
            self.server = None;
            self.content = None;
            self.content_type.clear();
            self.keep = 0;
            self.seen = None;
            match Server::start(want.0 as u16, want.1) {
                Ok(s) => {
                    self.status = format!("listening on http://{}/", s.addr);
                    self.error = None;
                    self.server = Some(s);
                }
                Err(e) => self.error = Some(e),
            }
        }
        let Some(server) = &self.server else {
            return Err(self.error.clone().unwrap_or_default());
        };
        let content = ctx.inputs.get(0).cloned().unwrap_or_default();
        let content_changed = self
            .content
            .as_ref()
            .is_none_or(|c| !Dat::ptr_eq(c, &content));
        let ty = p.str("content_type");
        let keep = p.i64("keep").clamp(1, 1000);
        if content_changed || ty != self.content_type || keep != self.keep {
            let mut s = lock(&server.state);
            s.content = content.clone();
            s.content_type.clear();
            s.content_type.push_str(ty);
            s.keep = keep as usize;
            drop(s);
            self.content = Some(content);
            self.content_type.clear();
            self.content_type.push_str(ty);
            self.keep = keep;
        }
        if let Ok(s) = server.state.try_lock()
            && self.seen != Some(s.seq)
        {
            self.seen = Some(s.seq);
            let mut t = Table::new(3);
            t.push_row(["method", "path", "body"]);
            for r in &s.requests {
                t.push_row(r.iter().cloned());
            }
            ctx.output.replace(Dat::table(t));
            self.status = format!("listening on http://{}/ · {} requests", server.addr, s.seq);
        }
        Ok(())
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn create_server() -> Box<dyn DatOp> {
    Box::<ServerOp>::default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::Harness;
    use serde_json::json;

    /// A free loopback port (bound and released: a small race at worst).
    pub(crate) fn free_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Cooks until `done` holds of the harness (workers answer
    /// asynchronously); returns the last cook's result.
    pub(crate) fn until(
        h: &mut Harness,
        op: &mut dyn DatOp,
        mut done: impl FnMut(&Harness, &Result<bool, String>) -> bool,
    ) -> Result<bool, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let r = h.cook(op);
            if done(h, &r) {
                return r;
            }
            assert!(
                Instant::now() < deadline,
                "timed out; last {r:?}, {:?}",
                h.text()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nhost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn headers_parse_one_per_line() {
        assert_eq!(
            parse_headers("Accept: text/plain\n\n X-A : b:c \n").unwrap(),
            [
                ("Accept".to_owned(), "text/plain".to_owned()),
                ("X-A".to_owned(), "b:c".to_owned())
            ]
        );
        assert!(parse_headers("nonsense").is_err());
    }

    #[test]
    fn the_server_serves_its_content_and_records_requests() {
        let port = free_port();
        let mut h = Harness::new(server_schema());
        let mut op = create_server();
        h.set("port", json!(port)).input(
            0,
            Some(Dat::table(Table::from_rows([["a", "b"], ["c", "d"]]))),
        );
        h.cook(op.as_mut()).unwrap();
        assert!(op.status().unwrap().contains(&format!("127.0.0.1:{port}")));
        assert_eq!(h.out.rows(), 1, "a header row and no requests");
        let page = get(port, "/anything");
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        assert!(page.contains("text/tab-separated-values"), "{page}");
        assert!(page.ends_with("a\tb\nc\td\n"), "{page}");
        // A POST is recorded with its body.
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        write!(s, "POST /cue HTTP/1.1\r\ncontent-length: 5\r\n\r\nhello").unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 3).unwrap();
        assert_eq!(h.out.as_table().row(2).unwrap(), ["POST", "/cue", "hello"]);
        assert_eq!(h.out.as_table().row(1).unwrap(), ["GET", "/anything", ""]);
        // Content type and keep.
        h.set("content_type", json!("csv")).set("keep", json!(1));
        h.cook(op.as_mut()).unwrap();
        let page = get(port, "/");
        assert!(
            page.contains("text/csv") && page.ends_with("a,b\nc,d\n"),
            "{page}"
        );
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 2).unwrap();
        // A port in use is a node error.
        let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut other = create_server();
        let mut h2 = Harness::new(server_schema());
        h2.set("port", json!(busy.local_addr().unwrap().port()));
        assert!(
            h2.cook(other.as_mut())
                .unwrap_err()
                .contains("cannot listen")
        );
    }

    #[test]
    fn the_client_fetches_from_the_server() {
        let port = free_port();
        let mut server = create_server();
        let mut hs = Harness::new(server_schema());
        hs.set("port", json!(port))
            .set("content_type", json!("json"))
            .input(0, Some(Dat::text("{\"cue\": 7}")));
        hs.cook(server.as_mut()).unwrap();

        let mut h = Harness::new(client_schema());
        let mut op = create_client();
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "", "no URL: nothing sent");
        h.set("url", json!(format!("http://127.0.0.1:{port}/state")));
        until(&mut h, op.as_mut(), |h, _| h.text() == "{\"cue\": 7}").unwrap();
        assert!(op.status().unwrap().starts_with("200"), "{:?}", op.status());
        // Not sent again while nothing changes.
        for _ in 0..20 {
            h.cook(op.as_mut()).unwrap();
        }
        hs.cook(server.as_mut()).unwrap();
        assert_eq!(hs.out.rows(), 2, "one request so far");
        // A POST with a body from the input and a header.
        h.set("method", json!("POST"))
            .set("headers", json!("X-Show: night"))
            .input(0, Some(Dat::text("go")));
        let deadline = Instant::now() + Duration::from_secs(10);
        while hs.out.rows() < 3 {
            h.cook(op.as_mut()).unwrap();
            hs.cook(server.as_mut()).unwrap();
            assert!(Instant::now() < deadline, "the POST never arrived");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(hs.out.as_table().row(2).unwrap(), ["POST", "/state", "go"]);
        // Bad input is a node error before anything is sent.
        h.set("headers", json!("nonsense"));
        assert!(h.cook(op.as_mut()).unwrap_err().contains("nonsense"));
        h.set("headers", json!("")).set("url", json!("ftp://x/"));
        assert!(h.cook(op.as_mut()).unwrap_err().contains("ftp"));
    }

    #[test]
    fn errors_and_sovereign_mode() {
        // Nothing listens there: a connection error.
        let port = free_port();
        let mut h = Harness::new(client_schema());
        let mut op = create_client();
        h.set("url", json!(format!("http://127.0.0.1:{port}/")));
        let r = until(&mut h, op.as_mut(), |_, r| r.is_err());
        assert!(r.unwrap_err().contains("127.0.0.1"));
        // Sovereign mode (on by default) refuses the internet before
        // anything is sent (a documentation address, never reached).
        assert!(atd_ai::egress::sovereign());
        h.set("url", json!("http://203.0.113.9/"));
        let r = until(&mut h, op.as_mut(), |_, r| {
            r.as_ref().is_err_and(|e| e.contains("Sovereign"))
        });
        assert!(r.unwrap_err().contains("203.0.113.9"));
    }
}
