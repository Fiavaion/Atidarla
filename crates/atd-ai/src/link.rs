//! The bridge between one AI node and the host running it.
//!
//! The render thread and a worker thread share two mailboxes: the latest
//! input frame (with the node's parameters) and the latest result. The
//! render thread only ever `try_lock`s them, copies into buffers that keep
//! their capacity, and moves on: it never waits for the worker, the network
//! or the host (rule 1), and allocates nothing once the buffers have their
//! size (rule 2). The worker routes the node (see [`crate::router`]), opens a
//! session through the [`Supervisor`], loads the model, keeps up to
//! `in_flight` frames submitted and publishes each result. When its host
//! fails it opens a new session (the supervisor restarts local hosts) or
//! falls back along the route; the node keeps showing its last result.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::client::{ClientError, FramePath, HostClient, Pending};
use crate::protocol::{FrameSubmit, ModelLoad, codes};
use crate::ring::RingLayout;
use crate::router::{self, Need, Route, Skip, Target};
use crate::supervisor::Supervisor;

/// One parameter value sent with a frame.
#[derive(Debug, Clone, PartialEq)]
pub enum PVal {
    F(f64),
    I(i64),
    B(bool),
    S(Arc<str>),
}

/// The parameters sent with every frame, in a list that keeps its capacity
/// so updating it each frame allocates nothing. Keys `prompts.<n>.text` and
/// `prompts.<n>.weight` become the `prompts` array of the protocol (Prompt
/// Blend, A58); other keys are sent as they are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameParams {
    entries: Vec<(&'static str, PVal)>,
}

impl FrameParams {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            entries: Vec::with_capacity(n),
        }
    }

    pub fn set(&mut self, key: &'static str, value: PVal) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => {
                if *v != value {
                    *v = value;
                }
            }
            None => self.entries.push((key, value)),
        }
    }

    pub fn get(&self, key: &str) -> Option<&PVal> {
        self.entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// The protocol's `params` object (worker thread only: it allocates).
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        let mut prompts: Vec<(u32, Option<Arc<str>>, f64)> = Vec::new();
        for (key, v) in &self.entries {
            if let Some(rest) = key.strip_prefix("prompts.") {
                let (n, field) = rest.split_once('.').unwrap_or((rest, ""));
                let n: u32 = n.parse().unwrap_or(0);
                let i = match prompts.iter().position(|p| p.0 == n) {
                    Some(i) => i,
                    None => {
                        prompts.push((n, None, 1.0));
                        prompts.len() - 1
                    }
                };
                match (field, v) {
                    ("text", PVal::S(s)) => prompts[i].1 = Some(s.clone()),
                    ("weight", PVal::F(w)) => prompts[i].2 = *w,
                    _ => {}
                }
                continue;
            }
            out.insert(
                (*key).to_owned(),
                match v {
                    PVal::F(f) => json!(f),
                    PVal::I(i) => json!(i),
                    PVal::B(b) => json!(b),
                    PVal::S(s) => json!(&**s),
                },
            );
        }
        if !prompts.is_empty() {
            prompts.sort_by_key(|p| p.0);
            let list: Vec<Value> = prompts
                .into_iter()
                .filter_map(|(_, text, w)| {
                    let text = text.filter(|t| !t.trim().is_empty())?;
                    (w > 0.0).then(|| json!({"text": &*text, "weight": w}))
                })
                .collect();
            out.insert("prompts".into(), Value::Array(list));
        }
        Value::Object(out)
    }
}

/// The model a node runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSpec {
    pub model_id: String,
    pub adapter: String,
    pub path: Option<String>,
    pub options: Value,
}

/// What a link does; changing it re-routes and reloads.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkConfig {
    pub route: Route,
    pub model: ModelSpec,
    /// Vendor needed for real time (`nvidia` for `win+nvidia`).
    pub realtime_vendor: Option<String>,
    /// Frames submitted at once (1–4).
    pub in_flight: u32,
    /// Latency over this for 3 s moves the node down its route; 0 = none.
    pub budget_ms: f64,
    /// Largest frame the node sends (sizes the shared-memory ring).
    pub max_size: (u32, u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    /// Choosing a host.
    Routing,
    /// Opening a session and loading the model.
    Loading,
    Running,
    /// No host can run it now; the reason.
    Waiting(String),
}

/// What the node shows about where and how it runs (FR-A7 badges).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkStatus {
    pub state: LinkState,
    pub target: Option<Target>,
    /// The host meets the real-time need (else interactive rates).
    pub realtime: bool,
    /// Submit to result, smoothed.
    pub latency_ms: f64,
    /// Time the host spent per frame, smoothed.
    pub host_ms: f64,
    /// Results per second, smoothed.
    pub ai_fps: f64,
    pub frames: u64,
    /// Results the similarity filter skipped.
    pub skipped: u64,
    /// Sessions lost so far.
    pub reconnects: u32,
    pub vram_mb: Option<u64>,
    /// USD spent on cloud hosts by this node.
    pub cost_usd: f64,
    pub error: Option<String>,
}

impl Default for LinkStatus {
    fn default() -> Self {
        Self {
            state: LinkState::Routing,
            target: None,
            realtime: false,
            latency_ms: 0.0,
            host_ms: 0.0,
            ai_fps: 0.0,
            frames: 0,
            skipped: 0,
            reconnects: 0,
            vram_mb: None,
            cost_usd: 0.0,
            error: None,
        }
    }
}

#[derive(Debug, Default)]
struct InSlot {
    seq: u64,
    timestamp_us: u64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    params: FrameParams,
}

/// A result: RGBA8, `width * height * 4` bytes.
#[derive(Debug, Default)]
pub struct OutSlot {
    /// The input frame it answers.
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub skipped: bool,
    /// When it arrived.
    pub at: Option<Instant>,
}

struct Shared {
    input: Mutex<InSlot>,
    wake: Condvar,
    output: Mutex<OutSlot>,
    out_seq: AtomicU64,
    status: Mutex<LinkStatus>,
    config: Mutex<(u64, LinkConfig)>,
    stop: AtomicBool,
}

/// One AI node's connection to its host.
pub struct Link {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").finish_non_exhaustive()
    }
}

impl Link {
    pub fn new(supervisor: &'static Supervisor, config: LinkConfig) -> Self {
        let shared = Arc::new(Shared {
            input: Mutex::default(),
            wake: Condvar::new(),
            output: Mutex::default(),
            out_seq: AtomicU64::new(0),
            status: Mutex::default(),
            config: Mutex::new((1, config)),
            stop: AtomicBool::new(false),
        });
        let worker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("atd-ai-link".into())
                .spawn(move || Worker::new(supervisor, shared).run())
                .expect("spawn AI link worker")
        };
        Self {
            shared,
            worker: Some(worker),
        }
    }

    /// Replaces the configuration (when the node's route, model or size
    /// changes); the worker re-routes and reloads.
    pub fn set_config(&self, config: LinkConfig) {
        let mut c = self.shared.config.lock().expect("config");
        if c.1 != config {
            c.0 += 1;
            c.1 = config;
            drop(c);
            self.shared.wake.notify_all();
        }
    }

    /// Offers a frame without waiting: `fill` writes `width * height * 4`
    /// bytes into a buffer that keeps its capacity. Returns false when the
    /// worker holds the mailbox this instant (the frame is dropped; the next
    /// one goes).
    pub fn offer(
        &self,
        width: u32,
        height: u32,
        timestamp_us: u64,
        params: &FrameParams,
        fill: impl FnOnce(&mut [u8]),
    ) -> bool {
        let Ok(mut slot) = self.shared.input.try_lock() else {
            return false;
        };
        let len = width as usize * height as usize * 4;
        slot.pixels.resize(len, 0);
        fill(&mut slot.pixels);
        slot.width = width;
        slot.height = height;
        slot.timestamp_us = timestamp_us;
        slot.params.clone_from(params);
        slot.seq += 1;
        drop(slot);
        self.shared.wake.notify_one();
        true
    }

    /// Sequence number of the newest result.
    pub fn result_seq(&self) -> u64 {
        self.shared.out_seq.load(Ordering::Acquire)
    }

    /// Calls `f` with the newest result if it is newer than `*seen` and the
    /// mailbox is free this instant; updates `*seen`.
    pub fn take(&self, seen: &mut u64, f: impl FnOnce(&OutSlot)) -> bool {
        let n = self.result_seq();
        if n <= *seen {
            return false;
        }
        let Ok(out) = self.shared.output.try_lock() else {
            return false;
        };
        f(&out);
        *seen = n;
        true
    }

    /// Where and how the node runs (allocates: call it a few times a second).
    pub fn status(&self) -> LinkStatus {
        self.shared.status.lock().expect("status").clone()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

struct InFlight {
    seq: u64,
    sent: Instant,
    pending: Pending,
}

struct Worker {
    supervisor: &'static Supervisor,
    shared: Arc<Shared>,
    session: Option<(HostClient, Target, String)>,
    config_gen: u64,
    config: Option<LinkConfig>,
    flight: VecDeque<InFlight>,
    sent_seq: u64,
    /// Targets passed over for a while (failed, over budget), with until when.
    excluded: Vec<(Target, Instant)>,
    over_budget_since: Option<Instant>,
    last_result: Option<Instant>,
    /// Inline frames are copied out of the mailbox before sending.
    scratch: Vec<u8>,
}

impl Worker {
    fn new(supervisor: &'static Supervisor, shared: Arc<Shared>) -> Self {
        Self {
            supervisor,
            shared,
            session: None,
            config_gen: 0,
            config: None,
            flight: VecDeque::new(),
            sent_seq: 0,
            excluded: Vec::new(),
            over_budget_since: None,
            last_result: None,
            scratch: Vec::new(),
        }
    }

    fn status(&self, f: impl FnOnce(&mut LinkStatus)) {
        f(&mut self.shared.status.lock().expect("status"));
    }

    fn drop_session(&mut self, why: Option<String>) {
        if self.session.take().is_some() {
            self.status(|s| {
                s.reconnects += 1;
                s.error = why.clone();
            });
        }
        self.flight.clear();
    }

    fn run(mut self) {
        while !self.shared.stop.load(Ordering::Acquire) {
            {
                let c = self.shared.config.lock().expect("config");
                if c.0 != self.config_gen {
                    self.config_gen = c.0;
                    self.config = Some(c.1.clone());
                    drop(c);
                    self.drop_session(None);
                }
            }
            if self.session.is_none() && !self.connect() {
                self.idle(Duration::from_millis(100));
                continue;
            }
            self.pump();
        }
    }

    fn idle(&self, d: Duration) {
        let g = self.shared.input.lock().expect("input");
        let _ = self.shared.wake.wait_timeout(g, d);
    }

    /// Routes, opens a session and loads the model. False when no host can
    /// run the node now.
    fn connect(&mut self) -> bool {
        let Some(cfg) = self.config.clone() else {
            return false;
        };
        let now = Instant::now();
        self.excluded.retain(|(_, until)| *until > now);
        for t in &cfg.route.0 {
            if !(t.is_cloud() && crate::egress::sovereign()) {
                self.supervisor.want(&t.host_name());
            }
        }
        self.status(|s| s.state = LinkState::Routing);
        let excluded: Vec<Target> = self.excluded.iter().map(|(t, _)| t.clone()).collect();
        let need = Need {
            adapter: &cfg.model.adapter,
            realtime_vendor: cfg.realtime_vendor.as_deref(),
        };
        let choice = match router::choose(&cfg.route, need, &self.supervisor.snapshot(), &excluded)
        {
            Ok(c) => c,
            Err(skipped) => {
                let why = describe(&skipped);
                self.status(|s| {
                    s.state = LinkState::Waiting(why.clone());
                    s.target = None;
                });
                return false;
            }
        };
        self.status(|s| {
            s.state = LinkState::Loading;
            s.target = Some(choice.target.clone());
            s.realtime = choice.realtime;
        });
        let host = choice.target.host_name();
        let slots = cfg.in_flight.clamp(1, 4) * 2;
        let layout = RingLayout {
            slot_count: slots,
            width: cfg.max_size.0.max(1),
            height: cfg.max_size.1.max(1),
        };
        let client = match self.supervisor.session(&host, FramePath::Shm(layout)) {
            Ok(c) => c,
            Err(why) => {
                self.exclude(&choice.target, Duration::from_secs(2), why);
                return false;
            }
        };
        let load = ModelLoad {
            model_id: cfg.model.model_id.clone(),
            adapter: cfg.model.adapter.clone(),
            path: cfg.model.path.clone(),
            options: cfg.model.options.clone(),
        };
        if let Err(e) = client.load_model(&load) {
            self.exclude(&choice.target, Duration::from_secs(5), e.to_string());
            return false;
        }
        let vram = self
            .supervisor
            .host(&host)
            .and_then(|h| h.caps)
            .and_then(|c| c.vram_mb);
        self.status(|s| {
            s.state = LinkState::Running;
            s.error = None;
            s.vram_mb = vram;
        });
        self.session = Some((client, choice.target, host));
        self.over_budget_since = None;
        true
    }

    fn exclude(&mut self, t: &Target, d: Duration, why: String) {
        log::warn!("AI node: {} unavailable: {why}", t.host_name());
        // Passing a target over only helps when the route has another; a
        // lone target is retried at once (a restarting local host, PT-9).
        if self.config.as_ref().is_some_and(|c| c.route.0.len() > 1) {
            self.excluded.push((t.clone(), Instant::now() + d));
        }
        self.status(|s| {
            s.state = LinkState::Waiting(why.clone());
            s.error = Some(why);
        });
    }

    /// Submits new input while there is room, and collects results.
    fn pump(&mut self) {
        let cap = self.config.as_ref().map_or(1, |c| c.in_flight.clamp(1, 4)) as usize;
        if self.flight.len() < cap
            && let Err(e) = self.submit_newest()
        {
            if e.is_fatal() {
                self.drop_session(Some(e.to_string()));
            }
            return;
        }
        // Collect the oldest result: wait for it when the pipeline is full,
        // else only glance and wait briefly for new input.
        let full = self.flight.len() >= cap;
        if self.flight.is_empty() {
            self.idle(Duration::from_millis(50));
            return;
        }
        let reply = if full {
            let f = self.flight.pop_front().expect("front");
            Some((f.seq, f.sent, f.pending.wait()))
        } else {
            match self.flight.front().and_then(|f| f.pending.try_take()) {
                Some(r) => {
                    let f = self.flight.pop_front().expect("front");
                    Some((f.seq, f.sent, r))
                }
                None => None,
            }
        };
        let Some((seq, sent, reply)) = reply else {
            // Nothing back yet: wait for new input or a moment.
            let g = self.shared.input.lock().expect("input");
            if g.seq == self.sent_seq {
                let _ = self.shared.wake.wait_timeout(g, Duration::from_millis(1));
            }
            return;
        };
        match reply.and_then(|r| {
            let (client, ..) = self.session.as_ref().ok_or(ClientError::Disconnected)?;
            client.check_result(seq, r)
        }) {
            Ok((result, payload)) => self.publish(seq, sent, &result, payload),
            Err(ClientError::Rpc(e)) if e.code == codes::BUSY || e.code == codes::STALE_FRAME => {}
            Err(ClientError::Rpc(e)) if e.code == codes::UNKNOWN_MODEL => {
                // A restarted host lost the model: reconnect and reload.
                self.drop_session(Some(e.to_string()));
            }
            Err(e) if e.is_fatal() => self.drop_session(Some(e.to_string())),
            Err(e) => self.status(|s| s.error = Some(e.to_string())),
        }
    }

    fn submit_newest(&mut self) -> Result<(), ClientError> {
        let Some((client, ..)) = self.session.as_mut() else {
            return Err(ClientError::Disconnected);
        };
        let Some(cfg) = &self.config else {
            return Ok(());
        };
        let cap = cfg.in_flight.clamp(1, 4);
        let (submit, inline) = {
            let input = self.shared.input.lock().expect("input");
            if input.seq == self.sent_seq || input.width == 0 {
                return Ok(());
            }
            let (w, h) = (input.width, input.height);
            let len = input.pixels.len();
            let k = (input.seq % u64::from(cap)) as u32;
            let mut submit = FrameSubmit {
                seq: input.seq,
                timestamp_us: input.timestamp_us,
                model_id: cfg.model.model_id.clone(),
                width: w,
                height: h,
                input_slot: None,
                output_slot: None,
                aux: Default::default(),
                params: input.params.to_json(),
            };
            let inline = match client.ring_mut() {
                Some(ring) => {
                    let l = ring.layout();
                    if w > l.width || h > l.height {
                        return Err(ClientError::Protocol(format!(
                            "frame {w}×{h} is larger than the ring ({}×{})",
                            l.width, l.height
                        )));
                    }
                    ring.frame_mut(2 * k)[..len].copy_from_slice(&input.pixels);
                    ring.set_seq(2 * k, input.seq);
                    submit.input_slot = Some(2 * k);
                    submit.output_slot = Some(2 * k + 1);
                    false
                }
                None => {
                    self.scratch.clear();
                    self.scratch.extend_from_slice(&input.pixels);
                    true
                }
            };
            self.sent_seq = input.seq;
            (submit, inline)
        };
        let pending = client.submit(&submit, inline.then_some(self.scratch.as_slice()))?;
        self.flight.push_back(InFlight {
            seq: submit.seq,
            sent: Instant::now(),
            pending,
        });
        Ok(())
    }

    fn publish(
        &mut self,
        seq: u64,
        sent: Instant,
        result: &crate::protocol::FrameResult,
        payload: Option<Vec<u8>>,
    ) {
        let Some((client, _, host)) = &self.session else {
            return;
        };
        let now = Instant::now();
        let len = result.width as usize * result.height as usize * 4;
        {
            let mut out = self.shared.output.lock().expect("output");
            out.pixels.resize(len, 0);
            match (&payload, client.ring(), result.output_slot) {
                (Some(p), ..) => out.pixels.copy_from_slice(p),
                (None, Some(ring), Some(slot)) => {
                    out.pixels.copy_from_slice(&ring.frame(slot)[..len]);
                }
                _ => return,
            }
            out.seq = seq;
            out.width = result.width;
            out.height = result.height;
            out.skipped = result.skipped;
            out.at = Some(now);
        }
        self.shared.out_seq.fetch_add(1, Ordering::AcqRel);
        let latency = now.duration_since(sent).as_secs_f64() * 1e3;
        let interval = self
            .last_result
            .map(|t| now.duration_since(t).as_secs_f64());
        self.last_result = Some(now);
        let cost = match (&self.session, interval) {
            (Some((_, t, host)), Some(dt)) if t.is_cloud() => router::spend().charge(host, dt),
            _ => 0.0,
        };
        let _ = host;
        let ewma = |old: f64, new: f64| {
            if old == 0.0 {
                new
            } else {
                old * 0.9 + new * 0.1
            }
        };
        let mut over = false;
        let budget = self.config.as_ref().map_or(0.0, |c| c.budget_ms);
        self.status(|s| {
            s.frames += 1;
            s.skipped += u64::from(result.skipped);
            s.latency_ms = ewma(s.latency_ms, latency);
            s.host_ms = ewma(s.host_ms, result.host_ms);
            if let Some(dt) = interval.filter(|d| *d > 0.0) {
                s.ai_fps = ewma(s.ai_fps, 1.0 / dt);
            }
            s.cost_usd += cost;
            s.error = None;
            over = budget > 0.0 && s.latency_ms > budget;
        });
        // Over the latency budget for 3 s: move down the route if there is
        // somewhere to go.
        if over && self.config.as_ref().is_some_and(|c| c.route.0.len() > 1) {
            let since = *self.over_budget_since.get_or_insert(now);
            if now.duration_since(since) > Duration::from_secs(3)
                && let Some((_, t, _)) = &self.session
            {
                let t = t.clone();
                self.exclude(
                    &t,
                    Duration::from_secs(30),
                    "over its latency budget".into(),
                );
                self.drop_session(Some("over its latency budget".into()));
            }
        } else {
            self.over_budget_since = None;
        }
    }
}

fn describe(skipped: &[(Target, Skip)]) -> String {
    if skipped.is_empty() {
        return "no route".into();
    }
    skipped
        .iter()
        .map(|(t, s)| {
            let why = match s {
                Skip::Sovereign => "blocked by Sovereign mode".to_owned(),
                Skip::SpendCap => "spend cap reached".to_owned(),
                Skip::NotReady(st) => st.clone(),
                Skip::NoAdapter => "cannot run this model".to_owned(),
                Skip::Excluded => "failed recently".to_owned(),
            };
            format!("{}: {why}", t.host_name())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_become_a_weighted_list() {
        let mut p = FrameParams::with_capacity(8);
        p.set("prompts.0.text", PVal::S("a forest".into()));
        p.set("prompts.0.weight", PVal::F(0.25));
        p.set("prompts.1.text", PVal::S("a city".into()));
        p.set("prompts.1.weight", PVal::F(0.75));
        p.set("prompts.2.text", PVal::S(" ".into()));
        p.set("prompts.2.weight", PVal::F(1.0));
        p.set("seed", PVal::I(7));
        let j = p.to_json();
        assert_eq!(
            j["prompts"],
            json!([{"text": "a forest", "weight": 0.25}, {"text": "a city", "weight": 0.75}])
        );
        assert_eq!(j["seed"], 7);
        // Updating in place keeps one entry per key.
        p.set("seed", PVal::I(8));
        assert_eq!(p.to_json()["seed"], 8);
        assert_eq!(p.entries.len(), 7);
    }
}
