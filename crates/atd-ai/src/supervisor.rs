//! Supervisor (FR-A2): starts AI Hosts on demand, watches them and restarts
//! local ones that die, and hands AI nodes sessions to them.
//!
//! Each host has a thread. A local host is one process started with
//! `--listen 127.0.0.1:0 --token <random> --parent-pid <this>`; it prints the
//! address it bound, the supervisor keeps a control session to it for
//! capabilities and a health check every second, and every AI node opens its
//! own session (frames through a shared-memory ring). A LAN or cloud host is
//! only connected to (frames on the socket). A dead process, a closed
//! control connection or two missed health checks mean the host failed: a
//! local host is started again at once (PT-9), a remote one is reconnected
//! with back-off. Nodes hold their last good frame meanwhile.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::client::{FramePath, HostClient, HostCommand};
use crate::protocol::{Capabilities, Health};

/// How long a local host may take to print its address.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_EVERY: Duration = Duration::from_secs(1);
/// Checks for a dead local process this often.
const TICK: Duration = Duration::from_millis(50);

/// What a host is and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKind {
    /// A process on this machine started by the supervisor.
    Local(HostCommand),
    /// A host listening elsewhere (LAN machine or cloud pod).
    Remote { addr: String, token: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    /// `local`, `lan:<name>` or `cloud:<name>`.
    pub name: String,
    pub kind: HostKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostState {
    /// Not needed yet.
    Idle,
    Starting,
    Ready,
    /// Failed and being started again.
    Restarting(String),
    /// Could not be reached; retried with back-off.
    Failed(String),
}

impl HostState {
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Restarting(_) => "restarting",
            Self::Failed(_) => "failed",
        }
    }
}

/// A host as the UI and the router see it.
#[derive(Debug, Clone)]
pub struct HostSnapshot {
    pub name: String,
    pub state: HostState,
    pub caps: Option<Capabilities>,
    pub health: Option<Health>,
    pub addr: Option<String>,
    pub restarts: u32,
    /// When the host last became ready.
    pub ready_at: Option<Instant>,
    pub pid: Option<u32>,
}

struct Entry {
    spec: HostSpec,
    wanted: bool,
    snap: HostSnapshot,
    token: Option<String>,
    /// Bumped when the entry is removed or replaced: its thread then exits.
    generation: u64,
}

#[derive(Default)]
struct Inner {
    hosts: Mutex<BTreeMap<String, Entry>>,
    wake: Condvar,
    generation: AtomicU64,
    stopping: AtomicBool,
}

/// Starts, watches and restarts AI Hosts.
pub struct Supervisor {
    inner: Arc<Inner>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").finish_non_exhaustive()
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            inner: Arc::default(),
            threads: Mutex::default(),
        }
    }

    /// The process-wide supervisor the AI nodes use.
    pub fn global() -> &'static Supervisor {
        static GLOBAL: OnceLock<Supervisor> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let s = Supervisor::new();
            s.set(HostSpec {
                name: "local".into(),
                kind: HostKind::Local(default_local_command()),
            });
            s
        })
    }

    /// Adds or replaces a host. A replaced host's process is stopped.
    pub fn set(&self, spec: HostSpec) {
        let mut hosts = self.inner.hosts.lock().expect("hosts");
        if hosts.get(&spec.name).is_some_and(|e| e.spec == spec) {
            return;
        }
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let wanted = hosts.get(&spec.name).is_some_and(|e| e.wanted);
        let token = match &spec.kind {
            HostKind::Local(_) => Some(random_token()),
            HostKind::Remote { token, .. } => token.clone(),
        };
        hosts.insert(
            spec.name.clone(),
            Entry {
                snap: HostSnapshot {
                    name: spec.name.clone(),
                    state: HostState::Idle,
                    caps: None,
                    health: None,
                    addr: None,
                    restarts: 0,
                    ready_at: None,
                    pid: None,
                },
                spec: spec.clone(),
                wanted,
                token,
                generation,
            },
        );
        drop(hosts);
        let inner = self.inner.clone();
        let name = spec.name.clone();
        let t = std::thread::Builder::new()
            .name(format!("atd-ai-host-{name}"))
            .spawn(move || run_host(&inner, &name, generation))
            .expect("spawn supervisor thread");
        self.threads.lock().expect("threads").push(t);
        self.inner.wake.notify_all();
    }

    /// Removes a host (stopping a local process).
    pub fn remove(&self, name: &str) {
        self.inner.hosts.lock().expect("hosts").remove(name);
        self.inner.wake.notify_all();
    }

    /// Asks for a host to be started (or connected) if it is not already.
    pub fn want(&self, name: &str) {
        if let Some(e) = self.inner.hosts.lock().expect("hosts").get_mut(name)
            && !e.wanted
        {
            e.wanted = true;
            self.inner.wake.notify_all();
        }
    }

    pub fn snapshot(&self) -> Vec<HostSnapshot> {
        self.inner
            .hosts
            .lock()
            .expect("hosts")
            .values()
            .map(|e| e.snap.clone())
            .collect()
    }

    pub fn host(&self, name: &str) -> Option<HostSnapshot> {
        self.inner
            .hosts
            .lock()
            .expect("hosts")
            .get(name)
            .map(|e| e.snap.clone())
    }

    /// Opens a new session to a ready host (starting it on demand). Local
    /// hosts get the shared-memory `path`; remote ones always `Inline`.
    /// Blocks on the connection, so call it from a worker thread.
    pub fn session(&self, name: &str, path: FramePath) -> Result<HostClient, String> {
        self.want(name);
        let (addr, token, local) = {
            let hosts = self.inner.hosts.lock().expect("hosts");
            let e = hosts
                .get(name)
                .ok_or_else(|| format!("no AI host named {name}"))?;
            match (&e.snap.state, &e.snap.addr) {
                (HostState::Ready, Some(addr)) => (
                    addr.clone(),
                    e.token.clone(),
                    matches!(e.spec.kind, HostKind::Local(_)),
                ),
                (HostState::Failed(why) | HostState::Restarting(why), _) => {
                    return Err(format!("{name} {}: {why}", e.snap.state.label()));
                }
                (state, _) => return Err(format!("{name} is {}", state.label())),
            }
        };
        let path = if local { path } else { FramePath::Inline };
        HostClient::connect(&addr, token.as_deref(), path).map_err(|e| e.to_string())
    }

    /// Stops every host (on exit).
    pub fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.hosts.lock().expect("hosts").clear();
        self.inner.wake.notify_all();
        for t in self.threads.lock().expect("threads").drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The local host command: `ATD_AI_HOST_CMD` (whitespace-separated), or the
/// development host in `ai-host/` run by its own virtual environment when
/// it has one (it holds torch), else `python3 -m atd_ai_host`.
pub fn default_local_command() -> HostCommand {
    if let Ok(cmd) = std::env::var("ATD_AI_HOST_CMD") {
        let mut parts = cmd.split_whitespace().map(std::ffi::OsString::from);
        if let Some(program) = parts.next() {
            return HostCommand {
                program,
                args: parts.collect(),
                env: vec![],
            };
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ai-host");
    let venv = if cfg!(windows) {
        root.join(".venv/Scripts/python.exe")
    } else {
        root.join(".venv/bin/python")
    };
    let python = if venv.exists() {
        venv.into_os_string()
    } else {
        std::env::var_os("ATD_PYTHON")
            .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into())
    };
    HostCommand::python_module(python, root.join("src"))
}

pub(crate) fn random_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = Sha256::new();
    h.update(format!(
        "{:?}{}{}{:p}",
        std::time::SystemTime::now(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        &h
    ));
    h.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Updates the entry if it is still this generation; false when the host
/// was removed or replaced (its thread should exit).
fn update(inner: &Inner, name: &str, generation: u64, f: impl FnOnce(&mut Entry)) -> bool {
    let mut hosts = inner.hosts.lock().expect("hosts");
    match hosts.get_mut(name) {
        Some(e) if e.generation == generation && !inner.stopping.load(Ordering::Acquire) => {
            f(e);
            true
        }
        _ => false,
    }
}

/// A running host: the process (local only) and the control session.
struct Running {
    child: Option<Child>,
    control: HostClient,
    missed: u32,
    last_health: Instant,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn run_host(inner: &Arc<Inner>, name: &str, generation: u64) {
    let mut running: Option<Running> = None;
    let mut backoff = Duration::from_millis(250);
    loop {
        // Wait until wanted (or gone).
        let (spec, token) = {
            let mut hosts = inner.hosts.lock().expect("hosts");
            loop {
                if inner.stopping.load(Ordering::Acquire) {
                    return;
                }
                match hosts.get(name) {
                    Some(e) if e.generation == generation => {
                        if e.wanted {
                            break (e.spec.clone(), e.token.clone());
                        }
                    }
                    _ => return,
                }
                hosts = inner.wake.wait_timeout(hosts, TICK).expect("hosts").0;
            }
        };
        if running.is_none() {
            let restarting = !update(inner, name, generation, |e| {
                if !matches!(e.snap.state, HostState::Restarting(_)) {
                    e.snap.state = HostState::Starting;
                }
            });
            if restarting {
                return;
            }
            match start(&spec, token.as_deref()) {
                Ok((r, addr, caps)) => {
                    let pid = r.child.as_ref().map(Child::id);
                    if !update(inner, name, generation, |e| {
                        e.snap.state = HostState::Ready;
                        e.snap.addr = Some(addr);
                        e.snap.caps = Some(caps);
                        e.snap.ready_at = Some(Instant::now());
                        e.snap.pid = pid;
                    }) {
                        return;
                    }
                    running = Some(r);
                    backoff = Duration::from_millis(250);
                }
                Err(why) => {
                    log::warn!("AI host {name}: {why}");
                    if !update(inner, name, generation, |e| {
                        e.snap.state = HostState::Failed(why);
                    }) {
                        return;
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        }
        // Watch the running host.
        std::thread::sleep(TICK);
        let r = running.as_mut().expect("running");
        let mut failed: Option<String> = None;
        if sovereign_blocks(&spec) {
            failed = Some("Sovereign mode was turned on".into());
        }
        if failed.is_none()
            && let Some(child) = &mut r.child
            && let Ok(Some(status)) = child.try_wait()
        {
            failed = Some(format!("the host process exited ({status})"));
        }
        if failed.is_none() && !r.control.alive() {
            failed = Some("the host closed its connection".into());
        }
        if failed.is_none() && r.last_health.elapsed() >= HEALTH_EVERY {
            r.last_health = Instant::now();
            match r
                .control
                .send("host.health", &serde_json::Value::Null, None, HEALTH_EVERY)
                .and_then(|p| p.wait())
            {
                Ok((v, _)) => {
                    r.missed = 0;
                    let health = serde_json::from_value(v).ok();
                    update(inner, name, generation, |e| e.snap.health = health);
                }
                Err(e) => {
                    r.missed += 1;
                    if r.missed >= 2 {
                        failed = Some(format!("two health checks failed ({e})"));
                    }
                }
            }
        }
        if let Some(why) = failed {
            log::warn!("AI host {name}: {why}; restarting");
            running = None;
            if !update(inner, name, generation, |e| {
                e.snap.state = HostState::Restarting(why);
                e.snap.restarts += 1;
                e.snap.addr = None;
                e.snap.pid = None;
            }) {
                return;
            }
        }
    }
}

/// A local process that is a cloud target (`cloud:decart`: the host itself
/// talks to the internet, beyond the egress gate) may not run in Sovereign
/// mode.
fn sovereign_blocks(spec: &HostSpec) -> bool {
    matches!(spec.kind, HostKind::Local(_))
        && spec.name.starts_with("cloud:")
        && crate::egress::sovereign()
}

fn start(spec: &HostSpec, token: Option<&str>) -> Result<(Running, String, Capabilities), String> {
    if sovereign_blocks(spec) {
        return Err(format!(
            "Sovereign mode blocks {}: it reaches a service on the internet",
            spec.name
        ));
    }
    let (child, addr) = match &spec.kind {
        HostKind::Local(cmd) => {
            let (child, addr) = spawn_local(cmd, token)?;
            (Some(child), addr)
        }
        HostKind::Remote { addr, .. } => (None, addr.clone()),
    };
    let mut running = Running {
        child,
        // Placeholder replaced below; a control session needs the address.
        control: HostClient::connect(&addr, token, FramePath::Inline).map_err(|e| e.to_string())?,
        missed: 0,
        last_health: Instant::now(),
    };
    let caps = running.control.capabilities().map_err(|e| e.to_string())?;
    running.last_health = Instant::now();
    Ok((running, addr, caps))
}

/// Starts a local host and reads the address it prints.
fn spawn_local(cmd: &HostCommand, token: Option<&str>) -> Result<(Child, String), String> {
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .args(["--listen", "127.0.0.1:0", "--parent-pid"])
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(t) = token {
        command.args(["--token", t]);
    }
    for (k, v) in &cmd.env {
        command.env(k, v);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {:?}: {e}", cmd.program))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let last_err = Arc::new(Mutex::new(String::new()));
    {
        let last_err = last_err.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::debug!("ai-host: {line}");
                let mut l = last_err.lock().expect("stderr");
                *l = line;
            }
        });
    }
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut tx = Some(tx);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(addr) = line.strip_prefix("atd-ai-host listening on ")
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(addr.trim().to_owned());
            } else {
                log::debug!("ai-host: {line}");
            }
        }
    });
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(addr) => return Ok((child, addr)),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                if let Ok(Some(status)) = child.try_wait() {
                    std::thread::sleep(TICK);
                    let why = last_err.lock().expect("stderr").clone();
                    return Err(format!("the host exited at start ({status}): {why}"));
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("the host did not start within {START_TIMEOUT:?}"));
                }
            }
        }
    }
}
