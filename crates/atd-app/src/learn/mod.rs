//! Learn mode (FR-L1): interactive quests whose steps check themselves
//! against the live project. Quests are text files in `learn/quests/`
//! (docs/learn.md); a step's check is a pure function of a read-only
//! [`Snapshot`], evaluated a few times a second.

pub mod check;
mod markdown;
pub mod progress;
pub mod quest;

use std::path::PathBuf;

use atd_core::graph::{Network, NodeId};
use atd_core::pack::PackSet;
use atd_gpu::engine::Engine;
use atd_stage::Stage;
use egui::{Color32, RichText, Ui};

use crate::project::ProjectState;
use check::{CookInfo, Snapshot};
use progress::Progress;
use quest::Quest;

/// Seconds between evaluations of the open quest's current step.
const TICK: f64 = 0.4;

/// What the panel asks the app to do (the panel does not own the project).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Start an empty scratch project.
    NewProject,
    /// Open this example project folder.
    OpenExample(PathBuf),
}

pub struct LearnPanel {
    quests: Vec<Quest>,
    progress: Progress,
    /// The quest shown, as an index into `quests`.
    open: Option<usize>,
    /// Steps of the open quest that are done.
    done: usize,
    show_hint: bool,
    last_tick: f64,
    request: Option<Request>,
    /// The Packs the open project uses, found when a project opens
    /// (`packs_for` is the project counter they were read at).
    packs: Vec<String>,
    packs_for: Option<u64>,
    /// A line of feedback (an example that could not be found, a save that
    /// failed).
    note: Option<String>,
}

impl std::fmt::Debug for LearnPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LearnPanel")
            .field("quests", &self.quests.len())
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl Default for LearnPanel {
    fn default() -> Self {
        let progress = Progress::user_file().map_or_else(Progress::in_memory, Progress::load);
        Self::with(quest::shipped(), progress)
    }
}

impl LearnPanel {
    pub fn with(quests: Vec<Quest>, progress: Progress) -> Self {
        Self {
            quests,
            progress,
            open: None,
            done: 0,
            show_hint: false,
            last_tick: f64::NEG_INFINITY,
            request: None,
            packs: Vec::new(),
            packs_for: None,
            note: None,
        }
    }

    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// The step the open quest waits on, if a quest is open and unfinished.
    fn current(&self) -> Option<&quest::Step> {
        self.open
            .and_then(|q| self.quests.get(q))
            .and_then(|q| q.steps.get(self.done))
    }

    /// Whether the app should build a snapshot now, and whether that
    /// snapshot needs the Stage model.
    pub fn wants_tick(&self, now: f64) -> Option<bool> {
        let step = self.current()?;
        (now - self.last_tick >= TICK).then(|| step.check.needs_stage())
    }

    /// Evaluates the open quest's current step against `snapshot`.
    pub fn tick(&mut self, now: f64, snapshot: &Snapshot<'_>) {
        self.last_tick = now;
        let Some(q) = self.open else { return };
        let quest = &self.quests[q];
        // A following step that needs the Stage model, which was not read
        // for this tick, waits for the next one.
        let mut done = self.done;
        progress::advance(quest, &mut done, snapshot);
        if done != self.done {
            self.done = done;
            self.show_hint = false;
            if self.progress.set(&quest.id, done)
                && let Err(e) = self.progress.save()
            {
                log::warn!("learn: could not save progress: {e}");
                self.note = Some(format!("Could not save your progress: {e}"));
            }
        }
    }

    /// Once per frame: evaluates the open quest's step when it is time.
    /// Reads the project, never writes it, and does nothing while no quest
    /// is open.
    pub fn frame(&mut self, now: f64, engine: &Engine, project: &ProjectState) {
        let Some(need_stage) = self.wants_tick(now) else {
            return;
        };
        if self.packs_for != Some(project.opened) {
            self.packs_for = Some(project.opened);
            self.packs = pack_instances(engine.network(), &project.meta.packs);
        }
        let stage = need_stage.then(|| Stage::read(engine.network(), engine.registry()));
        let cook = EngineCook(engine);
        let packs = std::mem::take(&mut self.packs);
        let snapshot = Snapshot {
            network: engine.network(),
            registry: engine.registry(),
            cook: &cook,
            stage: stage.as_ref(),
            mappings: &project.meta.mappings,
            pack_instances: &packs,
            project_dir: project.dir.as_deref(),
        };
        self.tick(now, &snapshot);
        self.packs = packs;
    }

    /// Whether the panel wants frames even when nothing else changes, so
    /// its ticks appear while the student works.
    pub fn is_active(&self) -> bool {
        self.current().is_some()
    }

    fn open_quest(&mut self, index: usize) {
        self.open = Some(index);
        self.done = self.progress.done(&self.quests[index].id);
        self.show_hint = false;
        self.last_tick = f64::NEG_INFINITY;
        self.note = None;
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.open {
                None => self.list_ui(ui),
                Some(q) => self.quest_ui(ui, q),
            });
    }

    fn list_ui(&mut self, ui: &mut Ui) {
        ui.heading("Learn");
        ui.label(
            "Short quests that teach Atidarla by doing. Each step watches your project and ticks \
             itself when you have done it.",
        );
        let finished = self
            .quests
            .iter()
            .filter(|q| self.progress.is_complete(q))
            .count();
        ui.weak(format!(
            "{finished} of {} quests finished",
            self.quests.len()
        ));
        ui.separator();
        let mut chosen = None;
        for (i, q) in self.quests.iter().enumerate() {
            let done = self.progress.done(&q.id).min(q.steps.len());
            ui.horizontal(|ui| {
                mark(
                    ui,
                    if done == q.steps.len() {
                        Mark::Done
                    } else {
                        Mark::Todo
                    },
                );
                if ui.link(RichText::new(&q.title).strong()).clicked() {
                    chosen = Some(i);
                }
            });
            ui.indent(("quest", i), |ui| {
                ui.label(&q.summary);
                ui.weak(format!(
                    "{} min · {} steps · {done} done",
                    q.minutes,
                    q.steps.len()
                ));
                if !q.prerequisites.is_empty() {
                    ui.weak(format!("Do first: {}", self.titles_of(&q.prerequisites)));
                }
            });
            ui.add_space(4.0);
        }
        if let Some(i) = chosen {
            self.open_quest(i);
        }
    }

    /// Titles of the quests with these slugs.
    fn titles_of(&self, slugs: &[String]) -> String {
        slugs
            .iter()
            .map(|s| {
                self.quests
                    .iter()
                    .find(|q| q.slug() == s)
                    .map_or(s.as_str(), |q| q.title.as_str())
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn quest_ui(&mut self, ui: &mut Ui, index: usize) {
        let quest = self.quests[index].clone();
        if ui.button("< All quests").clicked() {
            self.open = None;
            return;
        }
        ui.heading(&quest.title);
        ui.label(&quest.summary);
        ui.weak(format!("About {} minutes", quest.minutes));
        let total = quest.steps.len();
        ui.add(
            egui::ProgressBar::new(self.done as f32 / total as f32)
                .text(format!("{} of {total} steps", self.done)),
        );
        ui.horizontal_wrapped(|ui| {
            let (label, tip) = match &quest.start {
                Some(_) => (
                    "Open the example project",
                    "Opens the project this quest starts from",
                ),
                None => (
                    "Start in a new project",
                    "Starts an empty project (you are asked before unsaved work is lost)",
                ),
            };
            if ui.button(label).on_hover_text(tip).clicked() {
                self.note = None;
                self.request = Some(match &quest.start {
                    Some(rel) => match resolve_example(rel) {
                        Some(p) => Request::OpenExample(p),
                        None => {
                            self.note = Some(format!(
                                "Could not find {rel}. Open it with File, Open Project."
                            ));
                            return;
                        }
                    },
                    None => Request::NewProject,
                });
            }
            if ui
                .button("Restart quest")
                .on_hover_text("Forget your progress in this quest")
                .clicked()
            {
                self.done = 0;
                self.show_hint = false;
                self.last_tick = f64::NEG_INFINITY;
                self.progress.set(&quest.id, 0);
                let _ = self.progress.save();
            }
        });
        if let Some(note) = &self.note {
            ui.colored_label(Color32::from_rgb(230, 160, 60), note);
        }
        ui.separator();
        for (i, step) in quest.steps.iter().enumerate() {
            let n = i + 1;
            if i < self.done {
                ui.horizontal(|ui| {
                    mark(ui, Mark::Done);
                    ui.label(format!("{n}. {}", step.title));
                });
            } else if i == self.done {
                ui.horizontal(|ui| {
                    mark(ui, Mark::Now);
                    ui.label(RichText::new(format!("{n}. {}", step.title)).strong());
                });
                ui.indent(("step", i), |ui| {
                    markdown::show(ui, &step.text);
                    if let Some(hint) = &step.hint {
                        ui.checkbox(&mut self.show_hint, "Show a hint");
                        if self.show_hint {
                            ui.group(|ui| markdown::show(ui, hint));
                        }
                    }
                    ui.weak("Waiting for you to do this...");
                });
            } else {
                ui.horizontal(|ui| {
                    mark(ui, Mark::Todo);
                    ui.weak(format!("{n}. {}", step.title));
                });
            }
        }
        if self.done >= total {
            ui.separator();
            ui.label(RichText::new("Quest finished. Well done.").strong());
            if let Some(next) = (index + 1 < self.quests.len()).then_some(index + 1)
                && ui
                    .button(format!("Next: {}", self.quests[next].title))
                    .clicked()
            {
                self.open_quest(next);
            }
        }
    }
}

/// The engine's cook counts and errors, as checks see them.
struct EngineCook<'a>(&'a Engine);

impl CookInfo for EngineCook<'_> {
    fn cooks(&self, node: NodeId) -> u64 {
        self.0.stat(node).map_or(0, |s| s.cooks)
    }

    fn error(&self, node: NodeId) -> Option<&str> {
        self.0.error(node)
    }
}

/// The Packs a project's patch uses. A project with Packs opens expanded
/// (`project::open`): an instance `flash1` of a Pack shows as nodes named
/// `flash1__<inner name>`. An instance is a name prefix whose inner names
/// are exactly the nodes of one of the project's Packs.
pub fn pack_instances(net: &Network, packs: &PackSet) -> Vec<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut inner: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for n in net.nodes() {
        if let Some((instance, rest)) = n.name.split_once("__") {
            let first = rest.split("__").next().unwrap_or(rest);
            inner.entry(instance).or_default().insert(first);
        }
    }
    let mut found = BTreeSet::new();
    for names in inner.values() {
        for def in packs.iter() {
            let want: BTreeSet<&str> = def.network.nodes().map(|n| n.name.as_str()).collect();
            if want == *names {
                found.insert(def.name.clone());
            }
        }
    }
    found.into_iter().collect()
}

#[derive(Clone, Copy)]
enum Mark {
    Done,
    Now,
    Todo,
}

/// A small drawn mark: the default fonts have no check mark or arrow.
fn mark(ui: &mut Ui, kind: Mark) {
    use egui::{Role, Sense, Shape, Stroke, WidgetInfo, vec2};
    let size = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let (label, colour) = match kind {
        Mark::Done if ui.visuals().dark_mode => ("done", Color32::from_rgb(110, 200, 120)),
        Mark::Done => ("done", Color32::from_rgb(30, 130, 50)),
        Mark::Now => ("current step", ui.visuals().strong_text_color()),
        Mark::Todo => ("not yet", ui.visuals().weak_text_color()),
    };
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, label));
    let (c, r) = (rect.center(), size * 0.32);
    let painter = ui.painter();
    match kind {
        Mark::Done => {
            painter.add(Shape::line(
                vec![
                    c + vec2(-r, 0.0),
                    c + vec2(-r * 0.3, r * 0.75),
                    c + vec2(r, -r * 0.75),
                ],
                Stroke::new(2.0, colour),
            ));
        }
        Mark::Now => {
            painter.add(Shape::convex_polygon(
                vec![
                    c + vec2(-r * 0.6, -r),
                    c + vec2(-r * 0.6, r),
                    c + vec2(r, 0.0),
                ],
                colour,
                Stroke::NONE,
            ));
        }
        Mark::Todo => {
            painter.circle_stroke(c, r, Stroke::new(1.0, colour));
        }
    }
}

/// Finds an example project: next to the working directory, next to the
/// program (a release folder), or in the source tree this was built from.
pub fn resolve_example(rel: &str) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(5).map(PathBuf::from));
    }
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    roots.into_iter().map(|r| r.join(rel)).find(|p| p.is_dir())
}

#[cfg(test)]
mod tests;
