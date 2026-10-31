//! The Merge panel (FR-F3, docs/merge.md): a three-way merge of folders or
//! Git revisions, or a project left conflicted by the Git merge driver.
//!
//! Loading reads folders and runs `git`, so it happens on a worker thread
//! and the panel polls for the result (rule 1). Conflicts are resolved
//! through [`Session`], whose methods are what the buttons call; "Save
//! merged" stays disabled until every conflict has a resolution. Merging
//! into the open project applies the merged patch as one undo step.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_format::Project;
use atd_format::git::Source;
use atd_format::merge::{self, Merge, Resolution};
use atd_gpu::engine::Engine;
use egui::{Color32, RichText, Ui};

use crate::project::ProjectState;

/// One side of a diff or merge.
#[derive(Debug, Clone)]
pub enum Side {
    /// The open project as it would be saved, taken on the UI thread.
    Open(Box<Project>),
    Source(Source),
}

impl Side {
    /// `git:<rev>` (of `project`) or a folder.
    pub fn parse(text: &str, project: Option<&Path>) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("enter a project folder or git:<revision>".into());
        }
        if text.starts_with("git:") && project.is_none() {
            return Err(
                "git:<revision> needs the open project to be saved in a Git repository".into(),
            );
        }
        Ok(Self::Source(Source::parse(
            text,
            project.unwrap_or(Path::new(".")),
        )))
    }

    /// Reads the side; blocks (run it on a worker).
    pub fn load(self) -> Result<Project, String> {
        match self {
            Self::Open(p) => Ok(*p),
            Self::Source(s) => s.load().map_err(|e| format!("{s}: {e}")),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Open(_) => "the open project".into(),
            Self::Source(s) => s.to_string(),
        }
    }
}

/// The open project as saving would write it: the engine's patch with
/// portable paths, and its settings. A project with Packs is compared as
/// saved on disk (the engine holds it expanded).
pub fn open_side(engine: &Engine, state: &ProjectState) -> Result<Side, String> {
    if !state.meta.packs.is_empty() {
        let dir = state.dir.clone().ok_or(crate::project::PACKS_READ_ONLY)?;
        return Ok(Side::Source(Source::Folder(dir)));
    }
    let net = match &state.dir {
        Some(dir) => crate::project::portable_paths(engine, dir),
        None => engine.network().clone(),
    };
    let mut p = state.meta.clone();
    p.resolution = [engine.config().width, engine.config().height];
    p.networks.clear();
    p.networks.insert(atd_format::MAIN.to_owned(), net);
    Ok(Side::Open(Box::new(p)))
}

/// Where "Save merged" puts the result.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Into the open project, as one undo step (then save as usual).
    Open,
    /// Written to this folder in canonical form.
    Folder(PathBuf),
}

/// What [`Session::save`] did.
#[derive(Debug)]
pub enum Saved {
    Written(PathBuf),
    /// Apply this to the open project ([`apply_to_open`]).
    Apply(Box<Project>),
}

/// A merge being resolved.
#[derive(Debug)]
pub struct Session {
    pub merge: Merge,
    pub target: Target,
    /// What was merged, for the heading.
    pub title: String,
    /// The conflict shown in the editor.
    pub selected: usize,
    /// The value being typed for the selected conflict.
    pub edit: String,
    pub edit_error: Option<String>,
}

impl Session {
    fn new(merge: Merge, target: Target, title: String) -> Self {
        let mut s = Self {
            merge,
            target,
            title,
            selected: 0,
            edit: String::new(),
            edit_error: None,
        };
        s.select(0);
        s
    }

    /// Shows conflict `i` in the editor, its current value in the text box.
    pub fn select(&mut self, i: usize) {
        self.selected = i;
        self.edit_error = None;
        self.edit = self.merge.conflicts.get(i).map_or_else(String::new, |c| {
            let v = c.chosen().unwrap_or_else(|| c.ours.clone());
            merge::value_text(&c.slot, v.as_ref())
        });
    }

    /// The "Base", "Ours" and "Theirs" buttons.
    pub fn pick(&mut self, i: usize, r: Resolution) {
        self.merge.resolve(i, Some(r));
        if i == self.selected {
            self.select(i);
        }
    }

    /// "Use this value": the typed text for conflict `i`.
    pub fn type_value(&mut self, i: usize, text: &str) -> Result<(), String> {
        let slot = &self.merge.conflicts.get(i).ok_or("no such conflict")?.slot;
        let value = merge::parse_value(slot, text)?;
        self.merge.resolve(i, Some(Resolution::Value(value)));
        Ok(())
    }

    /// "Undo choice".
    pub fn clear(&mut self, i: usize) {
        self.merge.resolve(i, None);
    }

    /// Whether "Save merged" is enabled.
    pub fn can_save(&self) -> bool {
        self.merge.unresolved() == 0
    }

    /// "Save merged": writes the folder, or returns the project to apply
    /// to the open one. Refuses while a conflict is open.
    pub fn save(&self) -> Result<Saved, String> {
        match &self.target {
            Target::Folder(dir) => {
                merge::save(dir, &self.merge).map_err(|e| e.to_string())?;
                Ok(Saved::Written(dir.clone()))
            }
            Target::Open => self
                .merge
                .result()
                .map(|p| Saved::Apply(Box::new(p)))
                .map_err(|e| e.to_string()),
        }
    }
}

/// Makes the open project the merged one: the patch as one undo step
/// ("Merge"), the settings directly.
pub fn apply_to_open(
    engine: &mut Engine,
    state: &mut ProjectState,
    merged: Project,
) -> Result<(), String> {
    let net = merged
        .networks
        .get(atd_format::MAIN)
        .ok_or("the merged project has no main network")?;
    let edits = atd_format::diff::edits_between(engine.network(), net);
    if !edits.is_empty() {
        let before = crate::project::network_python(engine.network());
        engine
            .edit("Merge", edits)
            .map_err(|e| format!("the merged patch does not apply: {e}"))?;
        // Python from the other side has not been allowed: ask, listing it,
        // before any of the patch's network Python runs again.
        let after = crate::project::network_python(engine.network());
        if after.iter().any(|p| !before.contains(p)) {
            state.withdraw_python();
        }
    }
    engine.set_resolution(merged.resolution[0], merged.resolution[1]);
    let meta = &mut state.meta;
    meta.name = merged.name;
    meta.description = merged.description;
    meta.resolution = merged.resolution;
    meta.mappings = merged.mappings;
    meta.timeline = merged.timeline;
    meta.ai = merged.ai;
    meta.models = merged.models;
    Ok(())
}

type Job = Receiver<Result<Session, String>>;

/// The Merge window.
#[derive(Debug, Default)]
pub struct MergePanel {
    pub open: bool,
    /// Base, ours and theirs as typed: folders or `git:<rev>`; an empty
    /// "ours" is the open project.
    pub inputs: [String; 3],
    /// Output folder when ours is not the open project.
    pub out: String,
    /// A project folder left conflicted by the merge driver.
    pub conflicted: String,
    job: Option<Job>,
    pub session: Option<Session>,
    pub status: Option<(String, bool)>,
}

impl MergePanel {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Starts a three-way merge of `[base, ours, theirs]` on a worker.
    pub fn start_three(&mut self, sides: [Side; 3], target: Target) {
        let title = format!(
            "base {} · ours {} · theirs {}",
            sides[0].label(),
            sides[1].label(),
            sides[2].label()
        );
        self.spawn(move || {
            let [b, o, t] = sides;
            let (b, o, t) = (b.load()?, o.load()?, t.load()?);
            Ok(Session::new(merge::merge(&b, &o, &t), target, title))
        });
    }

    /// Opens a project with `# atidarla-conflict` blocks on a worker.
    pub fn start_conflicted(&mut self, dir: PathBuf) {
        self.spawn(move || {
            let m = merge::open_conflicted(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .ok_or_else(|| format!("{}: no merge conflicts found", dir.display()))?;
            let title = format!("conflicted project {}", dir.display());
            Ok(Session::new(m, Target::Folder(dir), title))
        });
    }

    fn spawn(&mut self, f: impl FnOnce() -> Result<Session, String> + Send + 'static) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        self.job = Some(rx);
        self.status = Some(("Loading…".into(), false));
    }

    /// Takes a finished load. Returns whether something changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the merge stopped unexpectedly".into()),
        };
        self.job = None;
        match result {
            Ok(s) => {
                let n = s.merge.conflicts.len();
                self.status = Some((
                    if n == 0 {
                        "Merged cleanly: nothing to resolve.".into()
                    } else {
                        format!("{n} conflict(s) to resolve.")
                    },
                    false,
                ));
                self.session = Some(s);
            }
            Err(e) => self.status = Some((e, true)),
        }
        true
    }

    /// Draws the window. Returns a merged project to apply to the open one.
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        engine: Option<&Engine>,
        state: &ProjectState,
    ) -> Option<Project> {
        if self.poll() {
            ctx.request_repaint();
        }
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut apply = None;
        egui::Window::new("Merge")
            .open(&mut open)
            .default_size([820.0, 560.0])
            .show(ctx, |ui| apply = self.ui(ui, engine, state));
        self.open = open;
        apply
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        engine: Option<&Engine>,
        state: &ProjectState,
    ) -> Option<Project> {
        let dir = state.dir.as_deref();
        ui.collapsing("Start a merge", |ui| {
            egui::Grid::new("atd_merge_inputs")
                .num_columns(2)
                .show(ui, |ui| {
                    for (i, label) in ["Base", "Ours", "Theirs"].into_iter().enumerate() {
                        ui.label(label);
                        let hint = if i == 1 {
                            "empty: the open project"
                        } else {
                            "folder or git:<revision>"
                        };
                        ui.add(
                            egui::TextEdit::singleline(&mut self.inputs[i])
                                .hint_text(hint)
                                .desired_width(420.0),
                        );
                        ui.end_row();
                    }
                    ui.label("Write to");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.out)
                            .hint_text("folder (when ours is not the open project)")
                            .desired_width(420.0),
                    );
                    ui.end_row();
                });
            if ui
                .add_enabled(!self.busy(), egui::Button::new("Merge"))
                .on_hover_text("Three-way merge by node ID; conflicts are listed below")
                .clicked()
            {
                match self.sides(engine, state, dir) {
                    Ok((sides, target)) => self.start_three(sides, target),
                    Err(e) => self.status = Some((e, true)),
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Conflicted project");
                ui.add(
                    egui::TextEdit::singleline(&mut self.conflicted)
                        .hint_text("folder left by the Git merge driver")
                        .desired_width(320.0),
                );
                let text = self.conflicted.trim().to_owned();
                if ui
                    .add_enabled(
                        !self.busy() && !text.is_empty(),
                        egui::Button::new("Open conflicted"),
                    )
                    .clicked()
                {
                    self.start_conflicted(PathBuf::from(text));
                }
            });
        });
        if let Some((msg, error)) = &self.status {
            if *error {
                ui.colored_label(Color32::from_rgb(230, 80, 80), format!("Error: {msg}"));
            } else {
                ui.label(msg.as_str());
            }
        }
        let session = self.session.as_mut()?;
        ui.separator();
        ui.label(RichText::new(&session.title).strong());
        for a in &session.merge.auto {
            ui.weak(format!("Auto: {}: {}", a.label, a.note));
        }
        let mut apply = None;
        let total = session.merge.conflicts.len();
        let open_count = session.merge.unresolved();
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} of {total} conflict(s) resolved",
                total - open_count
            ));
            let save = ui
                .add_enabled(session.can_save(), egui::Button::new("Save merged"))
                .on_disabled_hover_text("Resolve every conflict first");
            if save.clicked() {
                match session.save() {
                    Ok(Saved::Written(dir)) => {
                        self.status = Some((format!("Wrote {}", dir.display()), false));
                    }
                    Ok(Saved::Apply(p)) => apply = Some(*p),
                    Err(e) => self.status = Some((e, true)),
                }
            }
        });
        let session = self.session.as_mut()?;
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical()
                .id_salt("atd_merge_list")
                .show(&mut cols[0], |ui| {
                    let mut select = None;
                    for (i, c) in session.merge.conflicts.iter().enumerate() {
                        // A sign as well as colour: ✔ resolved, ● open.
                        let (mark, colour) = if c.is_resolved() {
                            ("✔", Color32::from_rgb(90, 170, 110))
                        } else {
                            ("●", Color32::from_rgb(230, 160, 40))
                        };
                        let text = RichText::new(format!("{mark} [{}] {}", c.kind.name(), c.label))
                            .color(colour);
                        if ui
                            .selectable_label(i == session.selected, text)
                            .on_hover_text(c.kind.describe())
                            .clicked()
                        {
                            select = Some(i);
                        }
                    }
                    if let Some(i) = select {
                        session.select(i);
                    }
                });
            conflict_editor(&mut cols[1], session);
        });
        apply
    }

    fn sides(
        &self,
        engine: Option<&Engine>,
        state: &ProjectState,
        dir: Option<&Path>,
    ) -> Result<([Side; 3], Target), String> {
        let base = Side::parse(&self.inputs[0], dir)?;
        let theirs = Side::parse(&self.inputs[2], dir)?;
        if self.inputs[1].trim().is_empty() {
            let engine = engine.ok_or("no engine")?;
            if !state.meta.packs.is_empty() {
                return Err(crate::project::PACKS_READ_ONLY.into());
            }
            return Ok(([base, open_side(engine, state)?, theirs], Target::Open));
        }
        let ours = Side::parse(&self.inputs[1], dir)?;
        let out = self.out.trim();
        if out.is_empty() {
            return Err("enter a folder to write the merged project to".into());
        }
        Ok(([base, ours, theirs], Target::Folder(PathBuf::from(out))))
    }
}

/// Base, ours and theirs of the selected conflict, with the buttons that
/// resolve it.
fn conflict_editor(ui: &mut Ui, s: &mut Session) {
    let i = s.selected;
    let Some(c) = s.merge.conflicts.get(i) else {
        ui.weak("No conflicts.");
        return;
    };
    ui.label(RichText::new(&c.label).strong());
    ui.weak(c.kind.describe());
    let mut pick = None;
    for (name, r, v) in [
        ("Base", Resolution::Base, &c.base),
        ("Ours", Resolution::Ours, &c.ours),
        ("Theirs", Resolution::Theirs, &c.theirs),
    ] {
        ui.horizontal(|ui| {
            let chosen = c.resolution.as_ref() == Some(&r);
            if ui.selectable_label(chosen, format!("Use {name}")).clicked() {
                pick = Some(r);
            }
            let text = merge::value_text(&c.slot, v.as_ref());
            let text = if text.is_empty() {
                "(none)".to_owned()
            } else {
                text
            };
            ui.add(egui::Label::new(RichText::new(text).monospace()).wrap());
        });
    }
    if let Some(r) = pick {
        s.pick(i, r);
    }
    ui.separator();
    ui.label("Or type a value (empty: none):");
    ui.add(
        egui::TextEdit::multiline(&mut s.edit)
            .code_editor()
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    ui.horizontal(|ui| {
        if ui.button("Use this value").clicked() {
            let text = s.edit.clone();
            s.edit_error = s.type_value(i, &text).err();
        }
        let resolved = s.merge.conflicts[i].is_resolved();
        if ui
            .add_enabled(resolved, egui::Button::new("Undo choice"))
            .clicked()
        {
            s.clear(i);
        }
    });
    if let Some(e) = &s.edit_error {
        ui.colored_label(Color32::from_rgb(230, 80, 80), e.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_format::merge::ConflictKind;

    const BASE: &str = r#"format = 1
next_id = 3

[[node]]
id = 0
name = "noise1"
type = "img.noise"
pos = [0.0, 0.0]
params.scale = 2.0

[[node]]
id = 1
name = "level1"
type = "img.level"
pos = [200.0, 0.0]
inputs.in = { node = 0, output = "out" }
params.gamma = 1.2
"#;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atd-app-merge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn folder(root: &Path, name: &str, net: &str) -> PathBuf {
        let dir = root.join(format!("{name}.atidarla"));
        let net = atd_format::network_from_str(net).unwrap();
        atd_format::save(&dir, &Project::with_main("show", net)).unwrap();
        dir
    }

    fn wait(panel: &mut MergePanel) {
        let start = std::time::Instant::now();
        while panel.busy() {
            panel.poll();
            assert!(start.elapsed().as_secs() < 30, "the merge never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// PRD M5 AC5: a conflicting merge resolves through the UI.
    #[test]
    fn a_conflicting_merge_resolves_through_the_panel() {
        let root = temp("ac5");
        let base = folder(&root, "base", BASE);
        let ours = folder(
            &root,
            "ours",
            &BASE
                .replace("params.scale = 2.0", "params.scale = 5.0")
                .replace("name = \"level1\"", "name = \"grade\""),
        );
        let theirs = folder(
            &root,
            "theirs",
            &BASE
                .replace("params.scale = 2.0", "params.scale = 9.0")
                .replace("name = \"level1\"", "name = \"final\"")
                .replace("params.gamma = 1.2", "params.gamma = 2.2"),
        );
        let out = root.join("merged.atidarla");

        // The panel's fields, then its Merge button.
        let mut panel = MergePanel {
            inputs: [
                base.display().to_string(),
                ours.display().to_string(),
                theirs.display().to_string(),
            ],
            out: out.display().to_string(),
            ..Default::default()
        };
        let state = ProjectState::untitled(&atd_core::graph::Network::default());
        let (sides, target) = panel.sides(None, &state, None).unwrap();
        assert_eq!(target, Target::Folder(out.clone()));
        panel.start_three(sides, target);
        wait(&mut panel);
        assert!(!panel.status.as_ref().unwrap().1, "{:?}", panel.status);
        let s = panel.session.as_mut().unwrap();
        let kinds: Vec<ConflictKind> = s.merge.conflicts.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ConflictKind::Name, ConflictKind::Param]);
        assert!(!s.can_save(), "Save merged starts disabled");
        assert!(
            s.save().is_err() && !out.exists(),
            "nothing written while unresolved"
        );

        // "Use Theirs" on the name, a typed value on the parameter.
        s.select(0);
        s.pick(0, Resolution::Theirs);
        assert!(!s.can_save());
        s.select(1);
        assert_eq!(s.edit, "5.0", "the editor starts at ours");
        assert!(
            s.type_value(1, "{ value = ").is_err(),
            "bad text is refused"
        );
        assert!(!s.can_save());
        s.type_value(1, "7.5").unwrap();
        assert!(s.can_save());
        // Undo and redo a choice.
        s.clear(1);
        assert!(!s.can_save());
        s.type_value(1, "7.5").unwrap();
        assert!(matches!(s.save().unwrap(), Saved::Written(d) if d == out));

        let saved = atd_format::load(&out).unwrap();
        let net = saved.main().unwrap();
        let id = atd_core::graph::NodeId;
        assert_eq!(
            net.node(id(0)).unwrap().params["scale"].value,
            serde_json::json!(7.5)
        );
        assert_eq!(net.node(id(1)).unwrap().name, "final");
        assert_eq!(
            net.node(id(1)).unwrap().params["gamma"].value,
            serde_json::json!(2.2),
            "theirs' disjoint change merged"
        );
        // Canonical: formatting it again changes nothing.
        let text = std::fs::read_to_string(out.join("networks/main.toml")).unwrap();
        assert_eq!(text, atd_format::network_to_string(net).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_conflicted_project_opens_in_the_panel() {
        let root = temp("conflicted");
        let dir = folder(&root, "show", BASE);
        let m = merge::merge_network_text(
            BASE,
            &BASE.replace("params.scale = 2.0", "params.scale = 5.0"),
            &BASE.replace("params.scale = 2.0", "params.scale = 9.0"),
        )
        .unwrap();
        std::fs::write(dir.join("networks/main.toml"), &m.text).unwrap();
        let mut panel = MergePanel::default();
        panel.start_conflicted(dir.clone());
        wait(&mut panel);
        let s = panel.session.as_mut().unwrap();
        assert_eq!(s.target, Target::Folder(dir.clone()));
        s.pick(0, Resolution::Ours);
        s.save().unwrap();
        let p = atd_format::load(&dir).unwrap();
        assert_eq!(
            p.main()
                .unwrap()
                .node(atd_core::graph::NodeId(0))
                .unwrap()
                .params["scale"]
                .value,
            serde_json::json!(5.0)
        );
        // A folder without conflicts says so.
        panel.start_conflicted(dir);
        wait(&mut panel);
        assert!(panel.status.as_ref().unwrap().1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Merging into the open project is one undo step.
    #[test]
    fn merging_into_the_open_project_is_one_undo_step() {
        let _flag = crate::project::tests::python_flag();
        let Some(mut engine) = crate::project::tests::engine() else {
            return;
        };
        let root = temp("open");
        let real = |scale: &str, name: &str| {
            BASE.replace("params.scale = 2.0", &format!("params.scale = {scale}"))
                .replace("name = \"level1\"", &format!("name = \"{name}\""))
        };
        let base = folder(&root, "base", &real("2.0", "level1"));
        let theirs = folder(&root, "theirs", &real("2.0", "final"));
        let net = atd_format::network_from_str(&real("5.0", "level1")).unwrap();
        engine.load(net.clone()).unwrap();
        let mut state = ProjectState::untitled(&net);
        state.meta.name = "show".into();
        let mut panel = MergePanel {
            inputs: [
                base.display().to_string(),
                String::new(),
                theirs.display().to_string(),
            ],
            ..Default::default()
        };
        let (sides, target) = panel.sides(Some(&engine), &state, None).unwrap();
        assert_eq!(target, Target::Open);
        panel.start_three(sides, target);
        wait(&mut panel);
        let s = panel.session.as_ref().unwrap();
        assert!(s.merge.is_clean(), "{:?}", s.merge.conflicts);
        let Saved::Apply(p) = s.save().unwrap() else {
            panic!("applies to the open project");
        };
        apply_to_open(&mut engine, &mut state, *p).unwrap();
        let n = |e: &Engine, id| {
            e.network()
                .node(atd_core::graph::NodeId(id))
                .cloned()
                .unwrap()
        };
        assert_eq!(n(&engine, 1).name, "final");
        assert_eq!(n(&engine, 0).params["scale"].value, serde_json::json!(5.0));
        assert_eq!(engine.history().undo_label(), Some("Merge"));
        engine.undo().unwrap();
        assert_eq!(
            n(&engine, 1).name,
            "level1",
            "one undo step reverts the merge"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Merging in Python nobody allowed (their Python expression) withdraws
    /// the allowance and asks; a merge that brings none leaves it.
    #[test]
    fn merging_in_python_asks_again() {
        let _flag = crate::project::tests::python_flag();
        let Some(mut engine) = crate::project::tests::engine() else {
            return;
        };
        let net = atd_format::network_from_str(BASE).unwrap();
        engine.load(net.clone()).unwrap();
        let mut state = ProjectState::untitled(&net);
        atd_py::script::allow(true);
        let merged = |text: &str| {
            let mut p = atd_format::Project::new("show");
            p.networks.insert(
                atd_format::MAIN.to_owned(),
                atd_format::network_from_str(text).unwrap(),
            );
            p
        };
        let calm = BASE.replace("params.scale = 2.0", "params.scale = 3.0");
        apply_to_open(&mut engine, &mut state, merged(&calm)).unwrap();
        assert!(
            atd_py::script::allowed() && !state.python_ask,
            "no new Python"
        );
        let theirs = BASE.replace(
            "params.scale = 2.0",
            "params.scale = { value = 2.0, expr = \"[2.0, 3.0][int(time) % 2]\" }",
        );
        apply_to_open(&mut engine, &mut state, merged(&theirs)).unwrap();
        assert!(
            !atd_py::script::allowed(),
            "their Python waits for a choice"
        );
        assert!(state.python_ask);
        assert_eq!(crate::project::network_python(engine.network()).len(), 1);
        atd_py::script::allow(false);
    }
}
