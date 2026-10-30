//! The Assistant panel (FR-G4, FR-G2): prompt to network, and "explain this
//! network or node", in the app.
//!
//! The model loop ([`atd_agent::assistant::build`]) runs on a worker thread
//! against a copy of the open patch (rule 1: the UI and render thread never
//! wait on a model). Its tool calls are staged on that copy; progress
//! (model turns, tool calls, refusals) streams back over a channel. When
//! the run ends the panel shows the staged changes as a diff, and nothing
//! touches the show until the user presses Accept, which applies them as one
//! undo step. Show Lock (`agent.toml`, FR-G3) is read again on a worker when
//! Accept is pressed; while it is engaged, Accept is refused and the panel
//! says why.
//!
//! Models: Ollama on this machine by default (A8); Anthropic only when the
//! user picks it, with the key from `ANTHROPIC_API_KEY` or the per-user
//! hosts file (`cloud:anthropic`), and refused in Sovereign mode by the
//! egress gate before any packet leaves.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use atd_agent::assistant::{self, Anthropic, Llm, Ollama};
use atd_agent::cues::{CueContext, CueQueue, apply_move};
use atd_agent::{Agent, NetworkWorkspace, Scope, ShowLock, Workspace, tools};
use atd_core::Registry;
use atd_core::graph::{Edit, Network, NodeId};
use atd_gpu::engine::Engine;
use egui::{Color32, RichText, Ui};
use serde_json::{Value, json};

use crate::canvas::View;
use crate::diff_view::{Compared, compared_ui};

const ERROR: Color32 = Color32::from_rgb(230, 80, 80);
/// Model turns per request (as `atidarla agent ask`).
const MAX_STEPS: u32 = 40;
/// Lines kept in the progress log.
const MAX_LOG: usize = 2000;
/// The hosts-file entry that holds an Anthropic key.
const ANTHROPIC_HOST: &str = "cloud:anthropic";

/// Where the model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Ollama on this machine (the default, A8).
    Ollama,
    /// Anthropic's API: opt-in, needs the internet.
    Anthropic,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama (this machine)",
            Self::Anthropic => "Anthropic (cloud, opt-in)",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Ollama => "qwen2.5-coder:7b",
            Self::Anthropic => "claude-opus-5-5",
        }
    }

    /// Why this provider cannot run now, if it cannot.
    fn refused(self) -> Option<&'static str> {
        (self == Self::Anthropic && atd_ai::egress::sovereign()).then_some(
            "Sovereign mode is on, so cloud models are refused. Use Ollama, or turn Sovereign \
             mode off in View > AI Hosts.",
        )
    }
}

/// A model, made on the worker (it may read the hosts file).
pub type MakeLlm = Box<dyn FnOnce() -> Result<Box<dyn Llm + Send>, String> + Send>;

/// The model the user picked.
fn make_llm(provider: Provider, model: String) -> MakeLlm {
    Box::new(move || -> Result<Box<dyn Llm + Send>, String> {
        match provider {
            Provider::Ollama => Ok(Box::new(Ollama::new(&model))),
            Provider::Anthropic => {
                if let Some(why) = provider.refused() {
                    return Err(why.into());
                }
                let key = std::env::var("ANTHROPIC_API_KEY")
                    .ok()
                    .filter(|k| !k.is_empty())
                    .or_else(|| {
                        let path = atd_ai::models::Store::default().hosts_path();
                        atd_ai::hosts::HostTokens::load(&path)
                            .ok()?
                            .get(ANTHROPIC_HOST)
                            .map(str::to_owned)
                    })
                    .ok_or(
                        "no Anthropic key: set ANTHROPIC_API_KEY, or add a `cloud:anthropic` \
                         token in View > AI Hosts",
                    )?;
                Ok(Box::new(Anthropic { key, model }))
            }
        }
    })
}

/// What the worker reports.
#[derive(Debug)]
pub enum Progress {
    /// A model turn started.
    Step(u32),
    /// The model called a tool.
    Tool { name: String, args: String },
    /// A tool refused the call (the model sees why and may repair).
    Refused { name: String, why: String },
    /// The model said something.
    Text(String),
    /// The run failed (the model could not be reached, a bad `agent.toml`).
    Failed(String),
    /// A build finished: its staged changes, for review.
    Staged(Box<Review>),
    /// An explanation finished.
    Explained(String),
}

/// A finished build, waiting for Accept or Reject.
#[derive(Debug)]
pub struct Review {
    pub request: String,
    /// The patch the run started from.
    pub base: Network,
    /// The staged edits, applied together on Accept.
    pub edits: Vec<Edit>,
    /// What each staged change is.
    pub notes: Vec<String>,
    /// The base and the base with the edits applied, compared.
    pub compared: Compared,
    pub summary: String,
}

/// The open patch as Accept sees it: the engine in the app, a network with
/// a history in tests.
/// The open show as a cue sees it: moves apply through the engine's undo.
struct LiveShow<'a>(&'a mut Engine);

impl Workspace for LiveShow<'_> {
    fn network(&self) -> &Network {
        self.0.network()
    }
    fn registry(&self) -> &Registry {
        self.0.registry()
    }
    fn apply(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String> {
        self.0.edit(label, edits).map_err(|e| e.to_string())
    }
    fn cook_errors(&mut self) -> Vec<(NodeId, String)> {
        Vec::new()
    }
}

pub trait Live {
    fn network(&self) -> &Network;
    /// Applies `edits` as one undo step.
    fn edit(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String>;
}

impl Live for Engine {
    fn network(&self) -> &Network {
        Engine::network(self)
    }

    fn edit(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String> {
        Engine::edit(self, label, edits).map_err(|e| e.to_string())
    }
}

/// Applies a review to the open patch as one undo step, unless Show Lock is
/// engaged or the patch changed since the run started.
pub fn accept(review: &Review, lock: &ShowLock, live: &mut dyn Live) -> Result<(), String> {
    lock.check_graph_edit().map_err(|e| {
        format!(
            "{e}. The assistant's changes stay staged; disengage Show Lock in the \
             project's agent.toml to accept them."
        )
    })?;
    if live.network() != &review.base {
        return Err(
            "The patch changed while the assistant worked, so its changes may not fit. Reject \
             them and run the request again."
                .into(),
        );
    }
    if review.edits.is_empty() {
        return Ok(());
    }
    let label: String = review.request.chars().take(40).collect();
    live.edit(&format!("Assistant: {label}"), review.edits.clone())
}

/// A model that reports each turn, tool call and refusal, and stops when
/// asked.
struct Reporting {
    inner: Box<dyn Llm + Send>,
    tx: Sender<Progress>,
    stop: Arc<AtomicBool>,
    /// Messages already reported.
    seen: usize,
    step: u32,
}

impl Llm for Reporting {
    fn chat(&mut self, messages: &[Value], tools: &[Value]) -> Result<Value, String> {
        for m in messages.iter().skip(self.seen) {
            if m["role"] == "tool"
                && let Some(why) = m["content"]
                    .as_str()
                    .and_then(|c| c.strip_prefix("error: "))
            {
                let _ = self.tx.send(Progress::Refused {
                    name: m["name"].as_str().unwrap_or("?").to_owned(),
                    why: why.to_owned(),
                });
            }
        }
        self.seen = messages.len();
        if self.stop.load(Ordering::Acquire) {
            return Err("stopped".into());
        }
        self.step += 1;
        let _ = self.tx.send(Progress::Step(self.step));
        let reply = self.inner.chat(messages, tools)?;
        if let Some(t) = reply["content"].as_str().filter(|t| !t.trim().is_empty()) {
            let _ = self.tx.send(Progress::Text(t.to_owned()));
        }
        for c in reply["tool_calls"].as_array().into_iter().flatten() {
            let args = match &c["arguments"] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            let _ = self.tx.send(Progress::Tool {
                name: c["name"].as_str().unwrap_or("?").to_owned(),
                args,
            });
        }
        // Count the reply too, so its tool results are the next new messages.
        self.seen += 1;
        Ok(reply)
    }

    fn name(&self) -> String {
        self.inner.name()
    }
}

/// Runs `request` against a copy of `base` and stages the result.
fn build_job(
    make: MakeLlm,
    base: Network,
    registry: Registry,
    request: String,
    tx: Sender<Progress>,
    stop: Arc<AtomicBool>,
) {
    let inner = match make() {
        Ok(l) => l,
        Err(e) => {
            let _ = tx.send(Progress::Failed(e));
            return;
        }
    };
    let mut llm = Reporting {
        inner,
        tx: tx.clone(),
        stop,
        seen: 0,
        step: 0,
    };
    let mut ws = NetworkWorkspace::new(registry);
    ws.network = base.clone();
    // Staging only: a person accepts in the app, so no Accept scope; no
    // files or Stage either (the copy has neither).
    let mut agent = Agent::new([Scope::Edit].into_iter().collect(), ShowLock::default());
    let full = format!(
        "{request}\n\n(You are in the app's Assistant panel: stage the changes with tools and \
         stop. accept_changes is not available here; a person reviews and accepts them.)"
    );
    let run = assistant::build(&mut llm, &mut agent, &mut ws, &full, MAX_STEPS);
    if let Some(e) = &run.llm_error {
        let _ = tx.send(Progress::Failed(format!("{}: {e}", llm.name())));
    }
    let (edits, notes) = agent.take_staged();
    let mut staged = base.clone();
    for e in &edits {
        if let Err(e) = staged.apply(&ws.registry, e.clone()) {
            let _ = tx.send(Progress::Failed(format!(
                "the staged changes do not apply: {e}"
            )));
            return;
        }
    }
    let project = |net: Network| atd_format::Project::with_main("patch", net);
    let compared = Compared::compute(
        "the open patch → with the assistant's changes".into(),
        project(base.clone()),
        project(staged),
    );
    let summary = format!(
        "{} model turn(s), {} tool call(s), {} refused, {} change(s) staged",
        run.steps,
        run.tool_calls,
        run.tool_errors,
        notes.len()
    );
    let _ = tx.send(Progress::Staged(Box::new(Review {
        request,
        base,
        edits,
        notes,
        compared,
        summary,
    })));
}

/// The messages that ask for an explanation of `net` (or one node of it),
/// grounded in the operator catalogue and the schemas of the operators it
/// uses: the corpus the operator docs are generated from.
pub fn explain_messages(net: &Network, registry: &Registry, node: Option<NodeId>) -> Vec<Value> {
    let mut system = String::from(
        "You explain node networks (patches) in Atidarla, a real-time visual instrument, to \
         the person who built them. Be concrete and brief: what the network makes, how signal \
         flows from node to node, what each important parameter does, and anything that looks \
         wrong. Use only the operator documentation below; do not invent parameters.\n\n\
         Operators (type — summary):\n",
    );
    for op in registry.iter() {
        if op.type_id.starts_with(atd_core::pack::TYPE_PREFIX) {
            continue;
        }
        let first = op.summary.split(". ").next().unwrap_or_default();
        let _ = writeln!(system, "{} — {}", op.type_id, first.trim_end_matches('.'));
    }
    let mut types: Vec<&str> = match node.and_then(|id| net.node(id)) {
        Some(n) => vec![n.type_id.as_str()],
        None => net.nodes().map(|n| n.type_id.as_str()).collect(),
    };
    types.sort_unstable();
    types.dedup();
    system.push_str("\nDocumentation of the operators in this network:\n");
    for t in types {
        if let Some(op) = registry.get(t) {
            let _ = writeln!(system, "{}", tools::schema_json(op));
        }
    }
    let text = atd_format::network_to_string(net).unwrap_or_else(|e| format!("({e})"));
    let ask = match node.and_then(|id| net.node(id)) {
        Some(n) => format!(
            "Explain the node `{}` ({}) and its part in this network:\n\n{text}",
            n.name, n.type_id
        ),
        None => format!("Explain this network:\n\n{text}"),
    };
    vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": ask}),
    ]
}

fn explain_job(make: MakeLlm, messages: Vec<Value>, tx: Sender<Progress>) {
    let result = make().and_then(|mut llm| {
        let _ = tx.send(Progress::Step(1));
        let r = llm.chat(&messages, &[])?;
        Ok(r["content"].as_str().unwrap_or_default().to_owned())
    });
    let _ = tx.send(match result {
        Ok(text) => Progress::Explained(text),
        Err(e) => Progress::Failed(e),
    });
}

/// What the tab asked for this frame (the app has the engine and the
/// project folder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Build,
    Cue,
    Explain(Option<NodeId>),
    Accept,
}

/// A line of the progress log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    Note,
    Tool,
    Error,
    Model,
}

/// The Assistant panel.
pub struct AssistantPanel {
    provider: Provider,
    pub model: String,
    pub prompt: String,
    /// The in-show cue being typed, a cue waiting on a fresh Show Lock
    /// read, and the moves queued for their beat (FR-G5).
    cue: String,
    cue_waiting: Option<String>,
    pub cues: CueQueue,
    cue_notes: Vec<String>,
    job: Option<Receiver<Progress>>,
    stop: Arc<AtomicBool>,
    log: Vec<(Line, String)>,
    pub review: Option<Review>,
    pub explanation: Option<String>,
    /// Show Lock as last read, and a read in flight.
    pub lock: Option<ShowLock>,
    lock_job: Option<Receiver<Result<ShowLock, String>>>,
    /// Accept was pressed; it applies when the lock read arrives.
    accepting: bool,
    pub status: Option<(String, bool)>,
    intent: Option<Intent>,
    view: View,
    fitted: bool,
}

impl Default for AssistantPanel {
    fn default() -> Self {
        Self {
            provider: Provider::Ollama,
            model: Provider::Ollama.default_model().into(),
            prompt: String::new(),
            cue: String::new(),
            cue_waiting: None,
            cues: CueQueue::default(),
            cue_notes: Vec::new(),
            job: None,
            stop: Arc::default(),
            log: Vec::new(),
            review: None,
            explanation: None,
            lock: None,
            lock_job: None,
            accepting: false,
            status: None,
            intent: None,
            view: View::default(),
            fitted: false,
        }
    }
}

impl std::fmt::Debug for AssistantPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantPanel")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}

/// Reads Show Lock from a project folder on a worker (no folder: no lock).
fn read_lock(dir: Option<PathBuf>) -> Receiver<Result<ShowLock, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(dir.map_or_else(|| Ok(ShowLock::default()), |d| ShowLock::load(&d)));
    });
    rx
}

impl AssistantPanel {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn note(&mut self, line: Line, text: impl Into<String>) {
        self.log.push((line, text.into()));
        if self.log.len() > MAX_LOG {
            self.log.drain(..self.log.len() - MAX_LOG);
        }
    }

    fn spawn(&mut self, f: impl FnOnce(Sender<Progress>) + Send + 'static) {
        let (tx, rx) = channel();
        std::thread::spawn(move || f(tx));
        self.job = Some(rx);
        self.status = None;
    }

    /// Starts a build of `request` on a worker, against a copy of `base`.
    /// A pending review is dropped (rejected).
    pub fn start_build(&mut self, make: MakeLlm, base: &Network, registry: &Registry) {
        let request = self.prompt.trim().to_owned();
        if request.is_empty() || self.busy() {
            return;
        }
        self.review = None;
        self.explanation = None;
        self.fitted = false;
        self.note(Line::Note, format!("Build: {request}"));
        let (base, registry) = (base.clone(), registry.clone());
        self.stop = Arc::default();
        let stop = self.stop.clone();
        self.spawn(move |tx| build_job(make, base, registry, request, tx, stop));
    }

    /// Starts an explanation of `net` (or `node`) on a worker.
    pub fn start_explain(
        &mut self,
        make: MakeLlm,
        net: &Network,
        registry: &Registry,
        node: Option<NodeId>,
    ) {
        if self.busy() {
            return;
        }
        let what = node
            .and_then(|id| net.node(id))
            .map_or("the network".to_owned(), |n| format!("`{}`", n.name));
        self.note(Line::Note, format!("Explain {what}"));
        self.explanation = None;
        let messages = explain_messages(net, registry, node);
        self.spawn(move |tx| explain_job(make, messages, tx));
    }

    /// Asks the running job to stop at the next model turn.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Drops the staged changes; the show is untouched.
    pub fn reject(&mut self) {
        if self.review.take().is_some() {
            self.accepting = false;
            self.status = Some(("Rejected: nothing changed.".into(), false));
            self.note(Line::Note, "Rejected the staged changes.");
        }
    }

    /// Accept pressed: reads Show Lock again (on a worker); the changes
    /// apply in [`Self::poll_accept`] once it arrives.
    pub fn request_accept(&mut self, dir: Option<&Path>) {
        if self.review.is_none() || self.lock_job.is_some() {
            return;
        }
        self.accepting = true;
        self.lock_job = Some(read_lock(dir.map(Path::to_owned)));
    }

    /// Re-reads Show Lock for the panel's status line.
    pub fn refresh_lock(&mut self, dir: Option<&Path>) {
        if self.lock_job.is_none() {
            self.lock_job = Some(read_lock(dir.map(Path::to_owned)));
        }
    }

    /// Takes worker progress. Returns whether anything arrived.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else {
            return false;
        };
        let mut events = Vec::new();
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(p) => events.push(p),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        let any = !events.is_empty() || done;
        for p in events {
            match p {
                Progress::Step(n) => self.note(Line::Note, format!("Model turn {n}…")),
                Progress::Tool { name, args } => {
                    let args: String = args.chars().take(160).collect();
                    self.note(Line::Tool, format!("{name} {args}"));
                }
                Progress::Refused { name, why } => {
                    self.note(Line::Error, format!("{name} refused: {why}"));
                }
                Progress::Text(t) => self.note(Line::Model, t),
                Progress::Failed(e) => {
                    self.note(Line::Error, e.clone());
                    self.status = Some((e, true));
                }
                Progress::Staged(r) => {
                    self.note(Line::Note, r.summary.clone());
                    self.status = Some((
                        if r.edits.is_empty() {
                            "The assistant staged no changes.".into()
                        } else {
                            "Review the staged changes, then Accept or Reject.".into()
                        },
                        false,
                    ));
                    self.review = (!r.edits.is_empty()).then_some(*r);
                }
                Progress::Explained(t) => {
                    self.explanation = Some(t);
                    self.status = None;
                }
            }
        }
        if done {
            self.job = None;
        }
        any
    }

    /// Takes a Show Lock read and, if Accept is waiting on it, applies the
    /// review to `live`. Returns whether anything changed.
    pub fn poll_accept(&mut self, live: &mut dyn Live) -> bool {
        let Some(rx) = &self.lock_job else {
            return false;
        };
        let read = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("reading agent.toml stopped".into()),
        };
        self.lock_job = None;
        let lock = match read {
            Ok(l) => l,
            Err(e) => {
                // An unreadable lock file must not let edits through.
                self.accepting = false;
                self.status = Some((format!("Show Lock: {e}"), true));
                return true;
            }
        };
        self.lock = Some(lock.clone());
        if !std::mem::take(&mut self.accepting) {
            return true;
        }
        let Some(review) = &self.review else {
            return true;
        };
        match accept(review, &lock, live) {
            Ok(()) => {
                let n = review.notes.len();
                self.review = None;
                self.status = Some((
                    format!("Accepted {n} change(s) as one undo step (Edit > Undo reverts)."),
                    false,
                ));
                self.note(Line::Note, "Accepted.");
            }
            Err(e) => self.status = Some((e, true)),
        }
        true
    }

    /// The app's per-frame work: progress, intents from the tab, Accept.
    /// Whether [`Self::frame`] needs the show's beat this frame (a cue is
    /// being planned or moves are queued); reading it costs a Stage read.
    pub fn wants_beat(&self) -> bool {
        self.cue_waiting.is_some()
            || !self.cues.is_empty()
            || matches!(self.intent, Some(Intent::Cue))
    }

    /// `beat` is the show's beat position (see [`Self::wants_beat`]).
    pub fn frame(
        &mut self,
        ctx: &egui::Context,
        engine: Option<&mut Engine>,
        dir: Option<&Path>,
        beat: Option<f64>,
    ) {
        if self.poll() {
            ctx.request_repaint();
        }
        if self.busy() || self.lock_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let Some(engine) = engine else {
            return;
        };
        match self.intent.take() {
            Some(Intent::Build) => {
                let make = make_llm(self.provider, self.model.clone());
                self.start_build(make, engine.network(), engine.registry());
                // Show the lock state alongside the run.
                self.refresh_lock(dir);
            }
            Some(Intent::Explain(node)) => {
                let make = make_llm(self.provider, self.model.clone());
                self.start_explain(make, engine.network(), engine.registry(), node);
            }
            Some(Intent::Accept) => self.request_accept(dir),
            Some(Intent::Cue) => {
                // Planned against the lock as it is on disk now.
                self.cue_waiting = Some(std::mem::take(&mut self.cue));
                self.refresh_lock(dir);
            }
            None => {}
        }
        if self.poll_accept(engine) {
            ctx.request_repaint();
        }
        if let Some(beat) = beat {
            self.cue_frame(&mut LiveShow(engine), beat);
            if !self.cues.is_empty() {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
    }

    /// Plans a waiting cue once the lock read is in, and applies the moves
    /// whose beat has come, each one undo step, checked again as they fire.
    pub fn cue_frame(&mut self, show: &mut dyn Workspace, beat: f64) {
        if self.lock_job.is_none()
            && let Some(text) = self.cue_waiting.take()
        {
            let lock = self.lock.clone().unwrap_or_default();
            let plan =
                CueContext::capture(show.network(), show.registry(), &lock).plan(&text, beat, None);
            self.cue_notes = plan.notes.clone();
            self.status = Some(if plan.moves.is_empty() {
                (format!("Cue \"{text}\" moves nothing."), true)
            } else {
                (
                    format!(
                        "Cue \"{text}\": {} move(s) at beat {:.0} (now {beat:.1}).",
                        plan.moves.len(),
                        plan.fire_at_beat
                    ),
                    false,
                )
            });
            self.cues.push(plan.moves);
        }
        if self.cues.is_empty() {
            return;
        }
        let due = self.cues.due(beat);
        if due.is_empty() {
            return;
        }
        let lock = self.lock.clone().unwrap_or_default();
        for m in &due {
            match apply_move(show, &lock, m) {
                Ok(v) => self.note(Line::Note, format!("Cue: {}.{} = {v:.3}", m.node, m.param)),
                Err(e) => self.note(Line::Error, format!("Cue refused: {e}")),
            }
        }
    }

    /// The tab. `selected` is the Patch selection; `errors` the open
    /// patch's node errors (for Repair).
    pub fn ui(&mut self, ui: &mut Ui, selected: Option<(NodeId, String)>, errors: &[String]) {
        egui::ScrollArea::vertical()
            .id_salt("atd_assistant")
            .show(ui, |ui| self.contents(ui, selected, errors));
    }

    fn contents(&mut self, ui: &mut Ui, selected: Option<(NodeId, String)>, errors: &[String]) {
        ui.horizontal(|ui| {
            ui.label("Model");
            let before = self.provider;
            egui::ComboBox::from_id_salt("atd_assistant_provider")
                .selected_text(self.provider.label())
                .show_ui(ui, |ui| {
                    for p in [Provider::Ollama, Provider::Anthropic] {
                        ui.selectable_value(&mut self.provider, p, p.label());
                    }
                });
            if self.provider != before {
                self.model = self.provider.default_model().into();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut self.model)
                    .desired_width(f32::INFINITY)
                    .hint_text("model name"),
            )
            .on_hover_text("The model's name at the provider");
        });
        let refused = self.provider.refused();
        if let Some(why) = refused {
            ui.colored_label(ERROR, why);
        }
        ui.add(
            egui::TextEdit::multiline(&mut self.prompt)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Describe a network to build, or what to change"),
        );
        let busy = self.busy();
        ui.horizontal_wrapped(|ui| {
            let can_run = !busy && refused.is_none();
            if ui
                .add_enabled(
                    can_run && !self.prompt.trim().is_empty(),
                    egui::Button::new("Build"),
                )
                .on_hover_text("Stage changes on a copy of the patch for review")
                .clicked()
            {
                self.intent = Some(Intent::Build);
            }
            let (text, node) = match &selected {
                Some((id, name)) => (format!("Explain {name}"), Some(*id)),
                None => ("Explain network".to_owned(), None),
            };
            if ui
                .add_enabled(can_run, egui::Button::new(text))
                .on_hover_text("Explain the selected node, or the whole network if none is")
                .clicked()
            {
                self.intent = Some(Intent::Explain(node));
            }
            if busy && ui.button("Stop").clicked() {
                self.stop();
            }
            if busy {
                ui.spinner();
            }
        });
        if !errors.is_empty() && !busy && refused.is_none() {
            let label = format!("Repair {} node error(s)", errors.len());
            if ui.button(label).on_hover_text(errors.join("\n")).clicked() {
                self.prompt = format!("These nodes have errors; fix them:\n{}", errors.join("\n"));
                self.intent = Some(Intent::Build);
            }
        }
        if let Some(lock) = &self.lock
            && lock.engaged
        {
            ui.colored_label(
                ERROR,
                "Show Lock is engaged (agent.toml): graph edits are refused during the show, \
                 so the assistant's changes cannot be accepted.",
            );
        }
        if let Some((msg, error)) = &self.status {
            if *error {
                ui.colored_label(ERROR, msg.as_str());
            } else {
                ui.label(msg.as_str());
            }
        }
        self.cue_ui(ui);
        if let Some(text) = &self.explanation {
            ui.separator();
            ui.strong("Explanation");
            ui.label(text.as_str());
        }
        let locked = self.lock.as_ref().is_some_and(|l| l.engaged);
        let mut reject = false;
        if let Some(review) = &mut self.review {
            ui.separator();
            ui.strong(format!("Staged changes ({})", review.notes.len()));
            for n in &review.notes {
                ui.label(format!("• {n}"));
            }
            ui.horizontal(|ui| {
                let accept = ui
                    .add_enabled(
                        !locked && self.lock_job.is_none(),
                        egui::Button::new("Accept"),
                    )
                    .on_hover_text("Apply every staged change as one undo step")
                    .on_disabled_hover_text(
                        "Show Lock is engaged: graph edits are refused during the show",
                    );
                if accept.clicked() {
                    self.intent = Some(Intent::Accept);
                }
                if ui
                    .button("Reject")
                    .on_hover_text("Drop the staged changes; nothing in the show changes")
                    .clicked()
                {
                    reject = true;
                }
            });
            compared_ui(ui, &mut review.compared, &mut self.view, &mut self.fitted);
        }
        if reject {
            self.intent = None;
            self.reject();
        }
        ui.separator();
        ui.collapsing("Progress", |ui| {
            if ui.small_button("Clear").clicked() {
                self.log.clear();
            }
            for (line, text) in &self.log {
                let t = RichText::new(text.as_str());
                ui.label(match line {
                    Line::Note => t.weak(),
                    Line::Tool => t.monospace(),
                    Line::Error => t.color(ERROR),
                    Line::Model => t,
                });
            }
        });
    }

    /// HOOK(cues): where in-show cue input will be wired once
    /// `atd_agent::cues` lands (another slice). The field is disabled and
    /// does nothing yet; `self.cue` is the text it will hand over.
    fn cue_ui(&mut self, ui: &mut Ui) {
        ui.collapsing("In-show cues", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.cue)
                    .hint_text("e.g. \"darker, slower on the next drop\""),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                let ready = !self.cue.trim().is_empty() && self.cue_waiting.is_none();
                if (ui.add_enabled(ready, egui::Button::new("Cue")).clicked() || enter) && ready {
                    self.intent = Some(Intent::Cue);
                }
                if !self.cues.is_empty() && ui.button("Cancel queued").clicked() {
                    self.cues.clear();
                }
            });
            ui.weak(
                "Moves only parameters whitelisted in agent.toml, within their ranges, on the \
                 next bar (\"now\": next beat; \"on the drop\": next 16 beats).",
            );
            for m in self.cues.pending() {
                ui.label(format!(
                    "• {}.{} → {:.3} at beat {:.0}",
                    m.node, m.param, m.target, m.fire_at_beat
                ));
            }
            for n in &self.cue_notes {
                ui.weak(n.as_str());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::history::History;

    /// A scripted model: replays replies, then stops.
    struct Script(Vec<Value>);

    impl Llm for Script {
        fn chat(&mut self, _: &[Value], _: &[Value]) -> Result<Value, String> {
            Ok(if self.0.is_empty() {
                json!({"content": "done", "tool_calls": []})
            } else {
                self.0.remove(0)
            })
        }
        fn name(&self) -> String {
            "script".into()
        }
    }

    fn call(name: &str, args: Value) -> Value {
        json!({"content": "", "tool_calls": [{"name": name, "arguments": args}]})
    }

    fn script(replies: Vec<Value>) -> MakeLlm {
        Box::new(move || Ok(Box::new(Script(replies)) as Box<dyn Llm + Send>))
    }

    /// The open patch: a network and its undo history, as the engine has.
    struct Fake {
        net: Network,
        registry: Registry,
        history: History,
    }

    impl Live for Fake {
        fn network(&self) -> &Network {
            &self.net
        }
        fn edit(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String> {
            self.history
                .commit(&mut self.net, &self.registry, label, edits)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }

    fn registry() -> Registry {
        let mut r = Registry::default();
        atd_ops_img::register(&mut r);
        r
    }

    /// noise1 → level1, as the user left it.
    fn fake() -> Fake {
        let net = atd_format::network_from_str(
            r#"
format = 1

[[node]]
id = 0
name = "noise1"
type = "img.noise"
pos = [0.0, 0.0]

[[node]]
id = 1
name = "level1"
type = "img.level"
pos = [200.0, 0.0]
inputs.in = { node = 0, output = "out" }
"#,
        )
        .unwrap();
        Fake {
            net,
            registry: registry(),
            history: History::default(),
        }
    }

    fn wait(panel: &mut AssistantPanel) {
        let start = std::time::Instant::now();
        while panel.busy() {
            panel.poll();
            assert!(
                start.elapsed().as_secs() < 30,
                "the assistant never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn wait_accept(panel: &mut AssistantPanel, live: &mut Fake) {
        let start = std::time::Instant::now();
        while panel.lock_job.is_some() {
            panel.poll_accept(live);
            assert!(
                start.elapsed().as_secs() < 30,
                "the lock read never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Adds a blur after level1 and changes a noise parameter.
    fn staged(live: &Fake) -> AssistantPanel {
        let mut panel = AssistantPanel {
            prompt: "blur it".into(),
            ..Default::default()
        };
        panel.start_build(
            script(vec![
                call("create_node", json!({"type": "img.blur", "name": "blur1"})),
                call(
                    "connect",
                    json!({"from": "level1", "to": "blur1", "input": "in"}),
                ),
                call(
                    "set_param",
                    json!({"node": "noise1", "param": "scale", "value": 4.0}),
                ),
                call("create_node", json!({"type": "img.nope"})),
            ]),
            &live.net,
            &live.registry,
        );
        wait(&mut panel);
        panel
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("atd-app-assistant-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_build_stages_on_a_copy_and_accept_is_one_undo_step() {
        let mut live = fake();
        let original = live.net.clone();
        let mut panel = staged(&live);
        // Progress streamed: turns, tool calls and the refusal.
        let log: Vec<&str> = panel.log.iter().map(|(_, t)| t.as_str()).collect();
        assert!(log.iter().any(|l| l.starts_with("create_node")), "{log:?}");
        assert!(log.iter().any(|l| l.contains("refused")), "{log:?}");
        assert!(log.iter().any(|l| l.starts_with("Model turn")), "{log:?}");
        let review = panel.review.as_ref().expect("changes are staged");
        assert_eq!(review.notes.len(), 3, "{:?}", review.notes);
        assert!(!review.compared.diff.is_empty());
        // Nothing touched the show before Accept.
        assert_eq!(live.net, original);
        assert!(!live.history.can_undo());

        panel.request_accept(None);
        wait_accept(&mut panel, &mut live);
        assert!(panel.review.is_none(), "{:?}", panel.status);
        assert_eq!(live.net.len(), 3);
        assert!(live.net.node_by_name("blur1").is_some());
        assert_eq!(
            live.history.undo_label(),
            Some("Assistant: blur it"),
            "one labelled step"
        );
        live.history.undo(&mut live.net, &live.registry).unwrap();
        // Nodes, wires and parameters are the original's (the ID counter
        // stays advanced: IDs are never reused).
        assert!(
            atd_format::diff::edits_between(&original, &live.net).is_empty(),
            "one undo restores the original"
        );
        assert_eq!(live.net.len(), 2);
        assert!(!live.history.can_undo(), "it was a single step");
    }

    #[test]
    fn reject_leaves_the_project_untouched() {
        let mut live = fake();
        let original = live.net.clone();
        let mut panel = staged(&live);
        assert!(panel.review.is_some());
        panel.reject();
        assert!(panel.review.is_none());
        // A late Accept has nothing to apply.
        panel.request_accept(None);
        assert!(panel.lock_job.is_none());
        panel.poll_accept(&mut live);
        assert_eq!(live.net, original);
        assert!(!live.history.can_undo());
    }

    #[test]
    fn show_lock_engaged_blocks_accept() {
        let mut live = fake();
        let original = live.net.clone();
        let dir = temp("lock");
        std::fs::write(dir.join("agent.toml"), "[show_lock]\nengaged = true\n").unwrap();
        let mut panel = staged(&live);
        panel.request_accept(Some(&dir));
        wait_accept(&mut panel, &mut live);
        let (msg, error) = panel.status.clone().unwrap();
        assert!(error && msg.contains("Show Lock is engaged"), "{msg}");
        assert!(panel.review.is_some(), "the changes stay staged");
        assert!(panel.lock.as_ref().is_some_and(|l| l.engaged));
        assert_eq!(live.net, original);
        assert!(!live.history.can_undo());
        // Disengaged, the same review is accepted.
        std::fs::write(dir.join("agent.toml"), "[show_lock]\nengaged = false\n").unwrap();
        panel.request_accept(Some(&dir));
        wait_accept(&mut panel, &mut live);
        assert!(panel.review.is_none(), "{:?}", panel.status);
        assert_eq!(live.net.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_typed_cue_moves_a_whitelisted_parameter_on_the_next_bar() {
        let mut live = fake();
        let dir = temp("cue");
        std::fs::write(
            dir.join("agent.toml"),
            "[show_lock]\nengaged = true\n\n[[show_lock.allow]]\nnode = \"level1\"\nparam = \"brightness\"\nmin = 0.2\nmax = 1.0\n",
        )
        .unwrap();
        let mut ws = NetworkWorkspace::new(registry());
        ws.network = live.net.clone();
        let before = ws.network.clone();
        let mut panel = AssistantPanel {
            cue_waiting: Some("darker, and delete noise1".into()),
            ..AssistantPanel::default()
        };
        panel.refresh_lock(Some(&dir));
        wait_accept(&mut panel, &mut live);
        assert!(panel.wants_beat());
        panel.cue_frame(&mut ws, 5.3);
        let pending = panel.cues.pending().to_vec();
        assert_eq!(pending.len(), 1, "{:?}", panel.status);
        assert_eq!(
            (pending[0].node.as_str(), pending[0].param.as_str()),
            ("level1", "brightness")
        );
        assert_eq!(pending[0].fire_at_beat, 8.0);
        panel.cue_frame(&mut ws, 7.9);
        assert_eq!(ws.network, before, "nothing moves before the bar");
        panel.cue_frame(&mut ws, 8.0);
        assert!(panel.cues.is_empty() && !panel.wants_beat());
        let v = ws.network.node_by_name("level1").unwrap().params["brightness"]
            .value
            .as_f64()
            .unwrap();
        assert!((0.2..1.0).contains(&v), "darker and within range: {v}");
        assert_eq!(ws.history.len(), 1, "one undo step");
        assert_eq!(ws.network.len(), before.len(), "no graph edit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_refuses_when_the_patch_changed_during_the_run() {
        let mut live = fake();
        let panel = staged(&live);
        live.edit(
            "user edit",
            vec![Edit::SetParam {
                node: NodeId(1),
                param: "contrast".into(),
                value: Some(atd_core::graph::Param::constant(json!(2.0))),
            }],
        )
        .unwrap();
        let review = panel.review.as_ref().unwrap();
        let e = accept(review, &ShowLock::default(), &mut live).unwrap_err();
        assert!(e.contains("changed while the assistant worked"), "{e}");
        assert_eq!(live.history.undo_label(), Some("user edit"));
    }

    #[test]
    fn a_model_that_cannot_be_reached_is_reported() {
        let live = fake();
        let mut panel = AssistantPanel {
            prompt: "anything".into(),
            ..Default::default()
        };
        panel.start_build(
            Box::new(|| Err("Ollama is not running".into())),
            &live.net,
            &live.registry,
        );
        wait(&mut panel);
        let (msg, error) = panel.status.clone().unwrap();
        assert!(error && msg.contains("not running"));
        assert!(panel.review.is_none());
    }

    #[test]
    fn explain_is_grounded_in_the_operator_docs_and_runs_off_thread() {
        let live = fake();
        let msgs = explain_messages(&live.net, &live.registry, Some(NodeId(1)));
        let system = msgs[0]["content"].as_str().unwrap();
        assert!(system.contains("img.noise — "), "the catalogue");
        assert!(system.contains("\"contrast\""), "level's schema");
        assert!(msgs[1]["content"].as_str().unwrap().contains("`level1`"));
        let mut panel = AssistantPanel::default();
        panel.start_explain(
            script(vec![
                json!({"content": "Noise, levelled.", "tool_calls": []}),
            ]),
            &live.net,
            &live.registry,
            None,
        );
        wait(&mut panel);
        assert_eq!(panel.explanation.as_deref(), Some("Noise, levelled."));
    }

    #[test]
    fn anthropic_is_refused_in_sovereign_mode() {
        let was = atd_ai::egress::sovereign();
        atd_ai::egress::set_sovereign(true);
        let made = make_llm(Provider::Anthropic, "m".into())();
        atd_ai::egress::set_sovereign(was);
        let e = made.err().expect("refused");
        assert!(e.contains("Sovereign mode"), "{e}");
    }
}
