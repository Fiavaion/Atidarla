//! The bridge between a Speech to Text node and the AI Host transcribing
//! for it (FR-A13): `audio.transcribe` with an adapter such as
//! `stt.whisper`.
//!
//! The render thread hands over one utterance at a time through a mailbox
//! whose buffer is sized once ([`SttLink::new`]); it only `try_lock`s, so
//! it never waits for the worker, the network or the host (rule 1), and an
//! utterance that fits the buffer is copied without allocating (rule 2).
//! An utterance stays in the mailbox until the worker takes it, so none is
//! overwritten while the host is busy: the node keeps recording and offers
//! again. The worker routes like an image node's [`crate::link::Link`]:
//! first ready host on the route with the adapter, cloud hosts skipped in
//! Sovereign mode, a new session after a failure.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::client::{ClientError, FramePath, HostClient};
use crate::link::{LinkState, describe};
use crate::protocol::{AudioTranscribe, ModelLoad, Transcript, codes};
use crate::router::{self, Need, Route, Skip, Target};
use crate::supervisor::Supervisor;

/// What the link runs; changing it re-routes and reloads.
#[derive(Debug, Clone, PartialEq)]
pub struct SttConfig {
    pub route: Route,
    pub model_id: String,
    /// `stt.whisper`, or `builtin` for `builtin.stt_echo`.
    pub adapter: String,
    /// The model folder (model manager cache), if the adapter needs one.
    pub path: Option<String>,
    /// Rate of the samples offered.
    pub sample_rate: u32,
    /// `audio.transcribe` params (`language`, `task`, `prompt`, …).
    pub params: Value,
}

/// Where the node runs and how it is doing.
#[derive(Debug, Clone, PartialEq)]
pub struct SttStatus {
    pub state: LinkState,
    pub target: Option<Target>,
    /// Utterance sent to transcript, smoothed.
    pub latency_ms: f64,
    /// Utterances transcribed.
    pub utterances: u64,
    /// A request is with the host now.
    pub busy: bool,
    /// Every target on the route is refused by policy (Sovereign mode or
    /// the spend cap).
    pub blocked: bool,
    pub error: Option<String>,
}

impl Default for SttStatus {
    fn default() -> Self {
        Self {
            state: LinkState::Routing,
            target: None,
            latency_ms: 0.0,
            utterances: 0,
            busy: false,
            blocked: false,
            error: None,
        }
    }
}

#[derive(Debug, Default)]
struct Mailbox {
    seq: u64,
    /// The utterance waits for the worker.
    full: bool,
    samples: Vec<f32>,
}

/// The newest transcript.
#[derive(Debug, Default)]
pub struct Heard {
    /// The utterance it answers.
    pub seq: u64,
    pub text: String,
    pub language: Option<String>,
    pub at: Option<Instant>,
}

struct Shared {
    input: Mutex<Mailbox>,
    wake: Condvar,
    output: Mutex<Heard>,
    out_seq: AtomicU64,
    status: Mutex<SttStatus>,
    config: Mutex<(u64, SttConfig)>,
    stop: AtomicBool,
}

/// One Speech to Text node's connection to its host.
pub struct SttLink {
    shared: Arc<Shared>,
    capacity: usize,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SttLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttLink").finish_non_exhaustive()
    }
}

impl SttLink {
    /// Starts the worker. `capacity` is the longest utterance in samples:
    /// the mailbox is allocated for it here, once.
    pub fn new(supervisor: &'static Supervisor, config: SttConfig, capacity: usize) -> Self {
        let shared = Arc::new(Shared {
            input: Mutex::new(Mailbox {
                seq: 0,
                full: false,
                samples: Vec::with_capacity(capacity),
            }),
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
                .name("lsr-ai-stt".into())
                .spawn(move || Worker::new(supervisor, shared, capacity).run())
                .expect("spawn speech-to-text worker")
        };
        Self {
            shared,
            capacity,
            worker: Some(worker),
        }
    }

    /// The longest utterance the mailbox takes without allocating.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn set_config(&self, config: SttConfig) {
        let mut c = self.shared.config.lock().expect("config");
        if c.1 != config {
            c.0 += 1;
            c.1 = config;
            drop(c);
            self.shared.wake.notify_all();
        }
    }

    /// Hands over an utterance without waiting. `None` when the worker holds
    /// the mailbox this instant or has not yet taken the previous utterance:
    /// keep it and offer again next frame. Samples beyond
    /// [`Self::capacity`] are dropped.
    pub fn offer(&self, samples: &[f32]) -> Option<u64> {
        let mut m = self.shared.input.try_lock().ok()?;
        if m.full {
            return None;
        }
        let n = samples.len().min(self.capacity);
        m.samples.clear();
        m.samples.extend_from_slice(&samples[..n]);
        m.full = true;
        m.seq += 1;
        let seq = m.seq;
        drop(m);
        self.shared.wake.notify_one();
        Some(seq)
    }

    /// Whether an utterance waits for the worker or is with the host.
    pub fn busy(&self) -> bool {
        self.shared.input.try_lock().map_or(true, |m| m.full) || self.with_status(|s| s.busy)
    }

    /// Sequence number of the newest transcript.
    pub fn result_seq(&self) -> u64 {
        self.shared.out_seq.load(Ordering::Acquire)
    }

    /// Calls `f` with the newest transcript if it is newer than `*seen` and
    /// the mailbox is free this instant; updates `*seen`.
    pub fn take(&self, seen: &mut u64, f: impl FnOnce(&Heard)) -> bool {
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

    /// Calls `f` with the status without copying it.
    pub fn with_status<R>(&self, f: impl FnOnce(&SttStatus) -> R) -> R {
        f(&self.shared.status.lock().expect("status"))
    }
}

impl Drop for SttLink {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

struct Worker {
    supervisor: &'static Supervisor,
    shared: Arc<Shared>,
    session: Option<HostClient>,
    config_gen: u64,
    config: Option<SttConfig>,
    excluded: Vec<(Target, Instant)>,
    /// The utterance as bytes for the payload.
    pcm: Vec<u8>,
}

impl Worker {
    fn new(supervisor: &'static Supervisor, shared: Arc<Shared>, capacity: usize) -> Self {
        Self {
            supervisor,
            shared,
            session: None,
            config_gen: 0,
            config: None,
            excluded: Vec::new(),
            pcm: Vec::with_capacity(capacity * 4),
        }
    }

    fn status(&self, f: impl FnOnce(&mut SttStatus)) {
        f(&mut self.shared.status.lock().expect("status"));
    }

    fn run(mut self) {
        while !self.shared.stop.load(Ordering::Acquire) {
            {
                let c = self.shared.config.lock().expect("config");
                if c.0 != self.config_gen {
                    self.config_gen = c.0;
                    self.config = Some(c.1.clone());
                    drop(c);
                    self.session = None;
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
        if !g.full {
            let _ = self.shared.wake.wait_timeout(g, d);
        }
    }

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
            adapter: &cfg.adapter,
            realtime_vendor: None,
        };
        let choice = match router::choose(&cfg.route, need, &self.supervisor.snapshot(), &excluded)
        {
            Ok(c) => c,
            Err(skipped) => {
                let why = describe(&skipped);
                let blocked = skipped
                    .iter()
                    .all(|(_, s)| matches!(s, Skip::Sovereign | Skip::SpendCap));
                self.status(|s| {
                    s.state = LinkState::Waiting(why);
                    s.target = None;
                    s.blocked = blocked;
                });
                return false;
            }
        };
        self.status(|s| {
            s.blocked = false;
            s.state = LinkState::Loading;
            s.target = Some(choice.target.clone());
        });
        let host = choice.target.host_name();
        // Audio travels on the socket: no frame ring is needed.
        let client = match self.supervisor.session(&host, FramePath::Inline) {
            Ok(c) => c,
            Err(why) => {
                self.exclude(&choice.target, Duration::from_secs(2), why);
                return false;
            }
        };
        let load = ModelLoad {
            model_id: cfg.model_id.clone(),
            adapter: cfg.adapter.clone(),
            path: cfg.path.clone(),
            options: Value::Object(serde_json::Map::new()),
        };
        if let Err(e) = client.load_model(&load) {
            self.exclude(&choice.target, Duration::from_secs(5), e.to_string());
            return false;
        }
        self.status(|s| {
            s.state = LinkState::Running;
            s.error = None;
        });
        self.session = Some(client);
        true
    }

    fn exclude(&mut self, t: &Target, d: Duration, why: String) {
        log::warn!("Speech to Text: {} unavailable: {why}", t.host_name());
        if self.config.as_ref().is_some_and(|c| c.route.0.len() > 1) {
            self.excluded.push((t.clone(), Instant::now() + d));
        }
        self.status(|s| {
            s.state = LinkState::Waiting(why.clone());
            s.error = Some(why);
        });
    }

    /// Takes the waiting utterance, if any, and transcribes it.
    fn pump(&mut self) {
        let seq = {
            let mut m = self.shared.input.lock().expect("input");
            if !m.full {
                let _ = self.shared.wake.wait_timeout(m, Duration::from_millis(100));
                return;
            }
            self.pcm.clear();
            for s in &m.samples {
                self.pcm.extend_from_slice(&s.to_le_bytes());
            }
            m.full = false;
            m.seq
        };
        let Some(cfg) = &self.config else { return };
        let Some(client) = &self.session else { return };
        let request = AudioTranscribe {
            seq,
            model_id: cfg.model_id.clone(),
            sample_rate: cfg.sample_rate,
            params: cfg.params.clone(),
        };
        self.status(|s| s.busy = true);
        let sent = Instant::now();
        let reply = client
            .transcribe(&request, &self.pcm)
            .and_then(|p| p.wait())
            .and_then(|(v, _)| {
                serde_json::from_value::<Transcript>(v)
                    .map_err(|e| ClientError::Protocol(e.to_string()))
            });
        self.status(|s| s.busy = false);
        match reply {
            Ok(t) => self.publish(seq, sent, t),
            Err(ClientError::Rpc(e)) if e.code == codes::UNKNOWN_MODEL => {
                // A restarted host lost the model: reconnect and reload.
                self.session = None;
                self.status(|s| s.error = Some(e.to_string()));
            }
            Err(e) if e.is_fatal() => {
                self.session = None;
                self.status(|s| s.error = Some(e.to_string()));
            }
            Err(e) => self.status(|s| s.error = Some(e.to_string())),
        }
    }

    fn publish(&mut self, seq: u64, sent: Instant, t: Transcript) {
        let now = Instant::now();
        {
            let mut out = self.shared.output.lock().expect("output");
            out.seq = seq;
            out.text = t.text;
            out.language = t.language;
            out.at = Some(now);
        }
        self.shared.out_seq.fetch_add(1, Ordering::AcqRel);
        let latency = now.duration_since(sent).as_secs_f64() * 1e3;
        self.status(|s| {
            s.utterances += 1;
            s.latency_ms = if s.latency_ms == 0.0 {
                latency
            } else {
                s.latency_ms * 0.8 + latency * 0.2
            };
            s.error = None;
        });
    }
}
