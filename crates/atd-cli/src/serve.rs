//! `atidarla serve` (FR-H3): a project without the UI, driven by agents and
//! controllers. A headless engine cooks the project at 60 frames a second
//! (outputs, sharing and DMX run as in the app); the agent's typed tools
//! (`atd_agent`) are offered over MCP on stdio (`--mcp-stdio`, for Claude
//! Code and other MCP clients) and over HTTP on loopback (`--http`: `POST
//! /mcp`, and REST `POST /api/tools/<tool>` with the arguments as the body);
//! OSC `/atd/<node>/<param> <value>` sets parameters (`--osc <port>`).
//! Everything goes through one agent session, so Show Lock (`agent.toml`)
//! and the session's scopes apply to every route. Accepted changes are
//! saved to the project folder (unless `--no-save`).

use std::collections::BTreeSet;
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use atd_agent::lock::Scope;
use atd_agent::mcp::{self, Request};
use atd_agent::{Agent, ShowLock, Workspace};
use atd_core::Registry;
use atd_core::graph::{Edit, Network, NodeId};
use atd_core::pack::Flat;
use atd_gpu::engine::{Engine, EngineConfig};
use serde_json::json;

use crate::project::{Loaded, load_checked};

#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// The project folder; created (empty) if it does not exist.
    pub project: PathBuf,
    /// Speak MCP on stdin/stdout (logs go to stderr).
    #[arg(long)]
    pub mcp_stdio: bool,
    /// Serve MCP and REST over HTTP on this loopback address, e.g.
    /// 127.0.0.1:7421.
    #[arg(long)]
    pub http: Option<String>,
    /// Receive OSC on this UDP port (loopback and LAN).
    #[arg(long)]
    pub osc: Option<u16>,
    /// Let the agent accept its own staged changes (no person reviews them).
    #[arg(long)]
    pub auto_accept: bool,
    /// Extra scopes for the session, e.g. `python.exec`.
    #[arg(long, value_delimiter = ',')]
    pub scope: Vec<String>,
    /// Do not write accepted changes to the project folder.
    #[arg(long)]
    pub no_save: bool,
    /// Stop after this many seconds (tests).
    #[arg(long)]
    pub seconds: Option<f64>,
}

/// The project as the agent sees it, cooked by a headless engine.
pub(crate) struct ServeWorkspace {
    dir: PathBuf,
    loaded: Loaded,
    engine: Engine,
    flat: Flat,
    save: bool,
    time: f64,
    pub history: Vec<String>,
}

impl ServeWorkspace {
    pub(crate) fn open(dir: &Path, save: bool) -> Result<Self> {
        if !dir.join("project.toml").exists() {
            let name = dir
                .file_stem()
                .map_or("Untitled".into(), |s| s.to_string_lossy().into_owned());
            std::fs::create_dir_all(dir)?;
            atd_format::save(dir, &atd_format::Project::new(name))?;
        }
        let gpu = atd_gpu::GpuContext::headless().map_err(|e| anyhow!("no GPU: {e}"))?;
        let mut loaded = load_checked(dir)?;
        let [w, h] = loaded.project.resolution;
        let registry = std::mem::take(&mut loaded.registry);
        let library = std::mem::take(&mut loaded.library);
        let mut engine = Engine::new(
            gpu.device.clone(),
            gpu.queue.clone(),
            registry,
            library,
            EngineConfig {
                width: w,
                height: h,
                format: wgpu::TextureFormat::Rgba8Unorm,
            },
        );
        engine.set_project_dir(Some(dir.to_owned()));
        engine.set_python(Box::new(atd_py::expr::PyExprHost::start()?));
        let main = loaded.project.main().cloned().unwrap_or_default();
        let flat = loaded
            .project
            .packs
            .flatten(&main, engine.registry())
            .map_err(|e| anyhow!("{e}"))?;
        engine.load(flat.network.clone())?;
        Ok(Self {
            dir: dir.to_owned(),
            loaded,
            engine,
            flat,
            save,
            time: 0.0,
            history: Vec::new(),
        })
    }

    /// Cooks every node once (one frame of the show).
    pub(crate) fn frame(&mut self) {
        let ids: Vec<NodeId> = self.engine.network().nodes().map(|n| n.id).collect();
        if let Err(e) = self.engine.cook(&ids, self.time) {
            log::warn!("cook: {e}");
        }
        self.time += 1.0 / 60.0;
    }

    fn reload(&mut self, net: Network) -> Result<(), String> {
        let flat = self
            .loaded
            .project
            .packs
            .flatten(&net, self.engine.registry())
            .map_err(|e| e.to_string())?;
        self.engine
            .load(flat.network.clone())
            .map_err(|e| e.to_string())?;
        self.flat = flat;
        self.loaded
            .project
            .networks
            .insert(atd_format::MAIN.to_owned(), net);
        if self.save {
            atd_format::save(&self.dir, &self.loaded.project).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

impl Workspace for ServeWorkspace {
    fn network(&self) -> &Network {
        static EMPTY: std::sync::OnceLock<Network> = std::sync::OnceLock::new();
        self.loaded
            .project
            .main()
            .unwrap_or_else(|| EMPTY.get_or_init(Network::default))
    }

    fn registry(&self) -> &Registry {
        self.engine.registry()
    }

    fn apply(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String> {
        let mut net = self.network().clone();
        for e in edits {
            net.apply(self.engine.registry(), e)
                .map_err(|e| e.to_string())?;
        }
        self.reload(net)?;
        self.history.push(label.to_owned());
        Ok(())
    }

    fn cook_errors(&mut self) -> Vec<(NodeId, String)> {
        // A few frames, so work started on worker threads can report.
        for _ in 0..4 {
            self.frame();
        }
        let mut out: Vec<(NodeId, String)> = Vec::new();
        for n in self.engine.network().nodes() {
            if let Some(e) = self.engine.error(n.id) {
                let top = self.flat.instance_of(n.id).unwrap_or(n.id);
                out.push((top, format!("{}: {e}", self.flat.display(n.id))));
            }
        }
        out
    }

    fn status(&self, node: NodeId) -> Option<String> {
        self.engine.status(self.flat.view(node)?).map(str::to_owned)
    }

    fn screenshot(&mut self, node: NodeId, max: u32) -> Result<Vec<u8>, String> {
        let view = self.flat.view(node).ok_or("that node has no image")?;
        self.engine
            .cook(&[view], self.time)
            .map_err(|e| e.to_string())?;
        if let Some(e) = self.engine.error(view) {
            return Err(format!("the node has an error: {e}"));
        }
        let tex = self
            .engine
            .result(view)
            .ok_or("that node has no image output (screenshots show IMG nodes)")?
            .clone();
        let px = atd_interop::readback::read_rgba8(self.engine.device(), self.engine.queue(), &tex)
            .map_err(|e| e.to_string())?;
        png_scaled(&px, tex.width(), tex.height(), max)
    }

    fn bench(&mut self, frames: u32) -> Result<Vec<(NodeId, f64)>, String> {
        for _ in 0..frames {
            self.frame();
        }
        let _ = self
            .engine
            .device()
            .poll(wgpu::PollType::wait_indefinitely());
        let mut by_top: std::collections::BTreeMap<NodeId, f64> = Default::default();
        for n in self.engine.network().nodes() {
            if let Some(s) = self.engine.stat(n.id) {
                let top = self.flat.instance_of(n.id).unwrap_or(n.id);
                *by_top.entry(top).or_default() += f64::from(s.avg_us) / 1e3;
            }
        }
        Ok(by_top.into_iter().collect())
    }

    fn project_dir(&self) -> Option<&Path> {
        Some(&self.dir)
    }

    fn files_changed(&mut self) -> Result<(), String> {
        // New kernel operators: open the project again (saving first so
        // nothing accepted is lost).
        if self.save {
            atd_format::save(&self.dir, &self.loaded.project).map_err(|e| e.to_string())?;
        }
        let fresh = Self::open(&self.dir, self.save).map_err(|e| format!("{e:#}"))?;
        let history = std::mem::take(&mut self.history);
        *self = fresh;
        self.history = history;
        Ok(())
    }
}

/// A PNG of RGBA pixels, scaled down (nearest) to at most `max` on the long side.
fn png_scaled(px: &[u8], w: u32, h: u32, max: u32) -> Result<Vec<u8>, String> {
    let s = (f64::from(max) / f64::from(w.max(h))).min(1.0);
    let (ow, oh) = (
        ((f64::from(w) * s) as u32).max(1),
        ((f64::from(h) * s) as u32).max(1),
    );
    let mut out = Vec::with_capacity((ow * oh * 4) as usize);
    for y in 0..oh {
        let sy = (u64::from(y) * u64::from(h) / u64::from(oh)) as usize;
        for x in 0..ow {
            let sx = (u64::from(x) * u64::from(w) / u64::from(ow)) as usize;
            let i = (sy * w as usize + sx) * 4;
            out.extend_from_slice(&px[i..i + 4]);
        }
    }
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, ow, oh);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&out).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

/// OSC `/atd/<node>/<param> <value>` → `set_param` through the session.
fn osc(port: u16, tx: Sender<Request>) -> Result<()> {
    let sock = UdpSocket::bind(("0.0.0.0", port)).with_context(|| format!("OSC port {port}"))?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok((n, _)) = sock.recv_from(&mut buf) {
            let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..n]) else {
                continue;
            };
            let mut msgs = Vec::new();
            flatten(packet, &mut msgs);
            for m in msgs {
                let parts: Vec<&str> = m.addr.trim_start_matches('/').split('/').collect();
                let ["atd", node, param] = parts.as_slice() else {
                    continue;
                };
                let value = match m.args.first() {
                    Some(rosc::OscType::Float(f)) => json!(f),
                    Some(rosc::OscType::Double(f)) => json!(f),
                    Some(rosc::OscType::Int(i)) => json!(i),
                    Some(rosc::OscType::Bool(b)) => json!(b),
                    Some(rosc::OscType::String(s)) => json!(s),
                    _ => continue,
                };
                let call = json!({"jsonrpc": "2.0", "id": 0, "method": "tools/call", "params": {
                    "name": "set_param", "arguments": {"node": node, "param": param, "value": value}}});
                let (rtx, _rrx) = channel();
                let _ = tx.send((call, rtx));
            }
        }
    });
    Ok(())
}

fn flatten(p: rosc::OscPacket, out: &mut Vec<rosc::OscMessage>) {
    match p {
        rosc::OscPacket::Message(m) => out.push(m),
        rosc::OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
    }
}

pub fn run(args: ServeArgs) -> Result<()> {
    if !args.mcp_stdio && args.http.is_none() && args.osc.is_none() {
        bail!("nothing to serve: give --mcp-stdio, --http <addr> and/or --osc <port>");
    }
    let mut ws = ServeWorkspace::open(&args.project, !args.no_save)?;
    let mut scopes: BTreeSet<Scope> = Scope::defaults();
    if args.auto_accept {
        scopes.insert(Scope::Accept);
    }
    for s in &args.scope {
        scopes.insert(Scope::parse(s).ok_or_else(|| anyhow!("unknown scope {s:?}"))?);
    }
    let lock = ShowLock::load(&args.project).map_err(|e| anyhow!(e))?;
    let mut agent = Agent::new(scopes, lock);

    let (tx, rx): (Sender<Request>, Receiver<Request>) = channel();
    let stdio = args.mcp_stdio.then(mcp::read_stdio);
    if let Some(addr) = &args.http {
        let listener = TcpListener::bind(addr).with_context(|| format!("listen on {addr}"))?;
        let local = listener.local_addr()?;
        if !local.ip().is_loopback() {
            bail!("--http listens on loopback only (got {local})");
        }
        eprintln!(
            "atidarla serve: MCP at http://{local}/mcp, REST at http://{local}/api/tools/<tool>"
        );
        mcp::serve_http(listener, tx.clone());
    }
    if let Some(port) = args.osc {
        osc(port, tx.clone())?;
        eprintln!("atidarla serve: OSC on udp port {port} (/atd/<node>/<param>)");
    }
    let start = Instant::now();
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    loop {
        let t = Instant::now();
        for source in [stdio.as_ref(), Some(&rx)].into_iter().flatten() {
            loop {
                match source.try_recv() {
                    Ok((msg, reply)) => {
                        let answer = mcp::handle(&mut agent, &mut ws, &msg);
                        let _ = reply.send(answer);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        // stdin closed: the MCP client is gone.
                        if stdio.as_ref().is_some_and(|s| std::ptr::eq(s, source)) {
                            return Ok(());
                        }
                        break;
                    }
                }
            }
        }
        ws.frame();
        if args
            .seconds
            .is_some_and(|s| start.elapsed().as_secs_f64() >= s)
        {
            return Ok(());
        }
        std::thread::sleep(frame.saturating_sub(t.elapsed()));
    }
}

/// For tests: a session over a project folder.
#[cfg(test)]
pub(crate) fn session(dir: &Path, scopes: BTreeSet<Scope>) -> Result<(Agent, ServeWorkspace)> {
    let ws = ServeWorkspace::open(dir, true)?;
    let lock = ShowLock::load(dir).map_err(|e| anyhow!(e))?;
    Ok((Agent::new(scopes, lock), ws))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn text(v: &Option<Value>) -> String {
        v.as_ref().unwrap()["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The MCP route end to end on a real headless engine: initialise, list
    /// tools, build noise → blur, accept, look at it, and the project on
    /// disk has the nodes.
    #[test]
    fn an_mcp_client_builds_and_saves_a_patch() {
        let Ok(gpu) = atd_gpu::GpuContext::headless() else {
            assert!(
                !std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return;
        };
        drop(gpu);
        let dir = std::env::temp_dir().join(format!("atd-serve-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scopes = Scope::defaults()
            .into_iter()
            .chain([Scope::Accept])
            .collect();
        let (mut agent, mut ws) = session(&dir, scopes).unwrap();
        let mut call = |method: &str, params: Value| {
            mcp::handle(
                &mut agent,
                &mut ws,
                &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
            )
        };
        let init = call("initialize", json!({"protocolVersion": mcp::PROTOCOL}));
        assert_eq!(init.unwrap()["result"]["serverInfo"]["name"], "atidarla");
        let tools = call("tools/list", json!({})).unwrap();
        assert!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "accept_changes")
        );
        for (name, args) in [
            (
                "create_node",
                json!({"type": "img.noise", "name": "noise1"}),
            ),
            ("create_node", json!({"type": "img.blur", "name": "blur1"})),
            (
                "connect",
                json!({"from": "noise1", "to": "blur1", "input": "in"}),
            ),
            (
                "set_param",
                json!({"node": "blur1", "param": "size", "value": 6.0}),
            ),
        ] {
            let r = call("tools/call", json!({"name": name, "arguments": args}));
            assert!(
                r.as_ref().unwrap()["result"]["isError"].is_null(),
                "{name}: {}",
                text(&r)
            );
        }
        let r = call(
            "tools/call",
            json!({"name": "accept_changes", "arguments": {"label": "noise blur"}}),
        );
        assert!(text(&r).contains("\"errors\": []"), "{}", text(&r));
        let shot = call(
            "tools/call",
            json!({"name": "screenshot_viewer", "arguments": {"node": "blur1", "max_size": 128}}),
        )
        .unwrap();
        assert_eq!(shot["result"]["content"][0]["type"], "image");
        drop(ws);
        let saved = atd_format::load(&dir).unwrap();
        let net = saved.main().unwrap();
        assert_eq!(net.len(), 2);
        assert!(net.node_by_name("blur1").is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
