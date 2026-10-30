//! Top-level application state and panel contents.

use std::time::Instant;

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::{Availability, OpFamily};
use atd_core::{CapabilityFlag, PlatformInfo, Registry};
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use egui::{Key, KeyboardShortcut, Modifiers, RichText, Ui};
use serde_json::json;

use crate::dock::{Area, DockState, TabViewer};
use crate::patch::PatchEditor;
use crate::project::{Action, ProjectState};
use crate::smoke::{Smoke, SmokeArgs};

const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);

/// Flags shown in the Platform panel so the capability mechanism is visible
/// before any flagged operator exists (PRD A7, A19).
const SAMPLE_FLAGS: [&str; 7] = [
    "win",
    "mac",
    "win+nvidia",
    "mac+apple",
    "nvidia",
    "win+sdk:kinect",
    "sdk:ndi",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Patch,
    Stage,
    Timeline,
    Parameters,
    Viewer,
    Operators,
    Platform,
    /// The Python console (docs/textport.md); closed by default.
    Textport,
}

pub struct AtidarlaApp {
    dock: DockState<Tab>,
    panels: Panels,
    smoke: Option<Smoke>,
    start: Instant,
    /// The latest uncaptured GPU error, shown until dismissed.
    gpu_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    project: ProjectState,
    /// Window title last sent, to send it only on change.
    title: String,
    /// The user confirmed quitting with unsaved changes.
    quitting: bool,
}

/// Everything the tabs render, split from the dock so both can be borrowed.
struct Panels {
    platform: PlatformInfo,
    adapter: Option<atd_gpu::AdapterReport>,
    engine: Option<Engine>,
    patch: PatchEditor,
    /// Node shown in the viewer: the selection, or the last selected node.
    viewed: Option<NodeId>,
    viewer: crate::viewer::TextureViewer,
    /// Where the viewed image was drawn this frame (for Input Devices).
    image_rect: Option<egui::Rect>,
    /// Wheel notches accumulated over the viewed image.
    wheel: f32,
    canvas_rect: egui::Rect,
    history: crate::channels::ChannelHistory,
    /// The time the engine cooked last, in seconds.
    time: f64,
    textport: crate::textport::TextportPanel,
    stage: crate::stage::StagePanel,
}

impl AtidarlaApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        smoke: Option<SmokeArgs>,
        open: Option<std::path::PathBuf>,
    ) -> Self {
        let rs = cc.wgpu_render_state.as_ref();
        let info = rs.map(|rs| rs.adapter.get_info());
        let platform = info
            .as_ref()
            .map_or_else(PlatformInfo::host, atd_gpu::platform_info);
        let adapter = info.as_ref().map(atd_gpu::AdapterReport::from_info);
        log::info!("platform: {platform:?}");
        if smoke.is_some() {
            println!(
                "{}",
                serde_json::json!({ "platform": platform, "adapter": adapter })
            );
        }

        // A GPU error (out of memory, a driver fault) must not end a show:
        // log it and show it, instead of wgpu's default panic.
        let gpu_error: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
        if let Some(rs) = rs {
            let slot = gpu_error.clone();
            rs.device
                .on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| {
                    log::error!("GPU error: {e}");
                    if let Ok(mut s) = slot.lock() {
                        *s = Some(e.to_string());
                    }
                }));
        }

        // The engine shares eframe's device, so the viewer shows engine
        // textures directly.
        let mut engine = rs.map(|rs| {
            let mut library = OpLibrary::default();
            atd_ops_img::library(&mut library);
            atd_ops_sig::library(&mut library);
            atd_ops_pnt::library(&mut library);
            atd_ops_mat::library(&mut library);
            atd_ops_msh::library(&mut library);
            let mut registry = builtin_registry();
            load_isf(&mut registry, &mut library);
            let mut engine = Engine::new(
                rs.device.clone(),
                rs.queue.clone(),
                registry,
                library,
                EngineConfig {
                    width: 1280,
                    height: 720,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                },
            );
            // Expressions outside the native subset run here (PRD A22).
            match atd_py::expr::PyExprHost::start() {
                Ok(host) => engine.set_python(Box::new(host)),
                Err(e) => log::warn!("Python expressions unavailable: {e}"),
            }
            engine
        });
        let mut viewed = engine.as_mut().and_then(demo_patch);
        let mut project = ProjectState::untitled(
            engine
                .as_ref()
                .map_or(&atd_core::graph::Network::default(), |e| e.network()),
        );
        if let (Some(engine), Some(dir)) = (engine.as_mut(), open) {
            viewed = project.run(engine, Action::OpenDir(dir));
            if let Some((msg, true)) = &project.message {
                log::error!("{msg}");
                if smoke.is_some() {
                    // Smoke runs must fail loudly on a project that does not load.
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                }
            }
        }

        let dock = DockState::new(
            vec![Tab::Operators, Tab::Platform],
            vec![Tab::Viewer, Tab::Parameters],
            vec![Tab::Timeline],
            vec![Tab::Patch, Tab::Stage],
        );
        Self {
            dock,
            gpu_error,
            panels: Panels {
                platform,
                adapter,
                engine,
                patch: PatchEditor::with_selection(viewed).frame_all(),
                viewed,
                viewer: Default::default(),
                image_rect: None,
                wheel: 0.0,
                canvas_rect: egui::Rect::NOTHING,
                history: Default::default(),
                time: 0.0,
                textport: Default::default(),
                stage: Default::default(),
            },
            smoke: smoke.map(Smoke::new),
            start: Instant::now(),
            project,
            title: String::new(),
            quitting: false,
        }
    }
}

/// A small starting network so a new user sees the engine working:
/// Noise → Kaleidoscope → Level, animated by two LFOs bound to Noise `z`
/// and Kaleidoscope `rotation`. Returns the node to view.
fn demo_patch(engine: &mut Engine) -> Option<NodeId> {
    let node = |engine: &mut Engine, ty: &str, name: &str, x: f32| {
        let id = engine.alloc_id();
        let mut n = Node::new(id, ty, name);
        n.pos = [x, 80.0];
        (id, n)
    };
    let (noise, n1) = node(engine, "img.noise", "noise1", 40.0);
    let (kal, n2) = node(engine, "img.kaleidoscope", "kaleidoscope1", 240.0);
    let (lvl, n3) = node(engine, "img.level", "level1", 440.0);
    let (drift, mut n4) = node(engine, "sig.lfo", "drift", 40.0);
    let (spin, mut n5) = node(engine, "sig.lfo", "spin", 240.0);
    n4.pos[1] = 260.0;
    n5.pos[1] = 260.0;
    let wire = |a: NodeId, b: NodeId| Edit::Connect {
        wire: Wire::new(Endpoint::new(a, "out"), Endpoint::new(b, "in")),
    };
    let set = |node: NodeId, p: &str, v: serde_json::Value| Edit::SetParam {
        node,
        param: p.into(),
        value: Some(Param::constant(v)),
    };
    let edits = vec![
        Edit::AddNode { node: n1 },
        Edit::AddNode { node: n2 },
        Edit::AddNode { node: n3 },
        Edit::AddNode { node: n4 },
        Edit::AddNode { node: n5 },
        wire(noise, kal),
        wire(kal, lvl),
        set(noise, "scale", json!(3.0)),
        set(noise, "monochrome", json!(false)),
        set(kal, "segments", json!(8)),
        set(lvl, "contrast", json!(1.3)),
        // drift: a slow ramp moving through the noise; spin: a gentle sway.
        set(drift, "waveform", json!("ramp")),
        set(drift, "frequency", json!(0.02)),
        set(drift, "amplitude", json!(5.0)),
        set(spin, "frequency", json!(0.05)),
        set(spin, "amplitude", json!(45.0)),
        Edit::SetParam {
            node: noise,
            param: "z".into(),
            value: Some(Param::bound(json!(0.0), drift, "lfo")),
        },
        Edit::SetParam {
            node: kal,
            param: "rotation".into(),
            value: Some(Param::bound(json!(0.0), spin, "lfo")),
        },
    ];
    match engine.edit("demo patch", edits) {
        Ok(()) => Some(lvl),
        Err(e) => {
            log::warn!("demo patch failed: {e}");
            None
        }
    }
}

impl std::fmt::Debug for AtidarlaApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtidarlaApp").finish_non_exhaustive()
    }
}

impl eframe::App for AtidarlaApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.shortcuts(ui);
        self.project_frame(ui.ctx());
        self.python_frame(ui.ctx());
        self.kernels_frame();
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| self.file_menu(ui));
                ui.menu_button("Edit", |ui| {
                    let (undo, redo) = self.panels.engine.as_ref().map_or((None, None), |e| {
                        (
                            e.history().undo_label().map(str::to_owned),
                            e.history().redo_label().map(str::to_owned),
                        )
                    });
                    let u = ui.add_enabled(
                        undo.is_some(),
                        egui::Button::new(format!("Undo {}", undo.unwrap_or_default())),
                    );
                    let r = ui.add_enabled(
                        redo.is_some(),
                        egui::Button::new(format!("Redo {}", redo.unwrap_or_default())),
                    );
                    if let Some(e) = &mut self.panels.engine {
                        if u.clicked() {
                            let _ = e.undo();
                        }
                        if r.clicked() {
                            let _ = e.redo();
                        }
                    }
                });
                ui.menu_button("View", |ui| {
                    for tab in [
                        Tab::Patch,
                        Tab::Stage,
                        Tab::Timeline,
                        Tab::Viewer,
                        Tab::Platform,
                    ] {
                        if ui.button(tab_title(tab)).clicked() {
                            self.dock.focus(|t| *t == tab);
                        }
                    }
                    ui.separator();
                    if ui
                        .button(tab_title(Tab::Textport))
                        .on_hover_text("Python console over the open project")
                        .clicked()
                    {
                        self.dock.open(Area::Bottom, Tab::Textport);
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("Atidarla {}", atd_core::VERSION));
                    if let Some((msg, error)) = self.project.message.clone() {
                        let short: String = msg.chars().take(80).collect();
                        ui.separator();
                        let label = if error {
                            ui.colored_label(egui::Color32::from_rgb(230, 80, 80), short)
                        } else {
                            ui.weak(short)
                        };
                        if label
                            .on_hover_text(format!("{msg}\n\nClick to dismiss."))
                            .clicked()
                        {
                            self.project.message = None;
                        }
                    }
                    let last = self.gpu_error.lock().ok().and_then(|g| g.clone());
                    if let Some(e) = last {
                        let short: String =
                            e.lines().next().unwrap_or("").chars().take(60).collect();
                        ui.separator();
                        if ui
                            .colored_label(
                                egui::Color32::from_rgb(230, 80, 80),
                                format!("GPU error: {short}"),
                            )
                            .on_hover_text(format!("{e}\n\nClick to dismiss."))
                            .clicked()
                            && let Ok(mut g) = self.gpu_error.lock()
                        {
                            *g = None;
                        }
                    }
                    ui.separator();
                    ui.weak(platform_summary(&self.panels.platform));
                });
            });
        });
        self.panels.image_rect = None;
        self.dock.show(ui, &mut self.panels);
        if std::mem::take(&mut self.panels.textport.close) {
            self.dock.close(|t| *t == Tab::Textport);
        }
        self.feed_input(ui.ctx());
        self.cook_and_present(ui.ctx(), frame);
        self.textport_frame(ui.ctx());
        if let Some(smoke) = &mut self.smoke {
            smoke.tick(ui.ctx());
        }
    }
}

impl AtidarlaApp {
    fn file_menu(&mut self, ui: &mut Ui) {
        let has_engine = self.panels.engine.is_some();
        let item = |ui: &mut Ui, text: &str, shortcut: &KeyboardShortcut| {
            ui.add_enabled(
                has_engine,
                egui::Button::new(text).shortcut_text(ui.ctx().format_shortcut(shortcut)),
            )
            .clicked()
        };
        if item(ui, "New Project", &NEW) {
            self.request(Action::New);
        }
        if item(ui, "Open Project…", &OPEN) {
            self.request(Action::Open);
        }
        ui.separator();
        if item(ui, "Save", &SAVE) {
            self.save();
        }
        if item(ui, "Save As…", &SAVE_AS) {
            self.project.save_as();
        }
        let blocked = self
            .project
            .python
            .as_ref()
            .is_some_and(|p| !p.is_allowed());
        if ui
            .add_enabled(blocked, egui::Button::new("Allow Python Operators…"))
            .on_hover_text("Run this project's ops/*.py (docs/python-ops.md)")
            .clicked()
        {
            self.project.python_ask = true;
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn request(&mut self, action: Action) {
        if let Some(engine) = &mut self.panels.engine {
            let view = self.project.request(engine, action);
            self.show_loaded(view);
        }
    }

    fn save(&mut self) {
        if let Some(engine) = &mut self.panels.engine {
            self.project.save(engine);
        }
    }

    /// After a load: view and select `view`, forget the old selection.
    fn show_loaded(&mut self, view: Option<NodeId>) {
        let loaded = self.project.confirm.is_none()
            && self.panels.engine.as_ref().is_some_and(|e| {
                self.panels
                    .viewed
                    .is_none_or(|v| e.network().node(v).is_none())
                    || view.is_some()
            });
        if loaded {
            self.panels.patch = PatchEditor::with_selection(view).frame_all();
            self.panels.viewed = view;
        }
    }

    /// Dialog results, the window title, closing with unsaved changes, and
    /// the "discard changes?" prompt.
    fn project_frame(&mut self, ctx: &egui::Context) {
        let Some(engine) = &mut self.panels.engine else {
            return;
        };
        if let Some(view) = self.project.poll(engine) {
            self.show_loaded(Some(view));
        }
        let engine = self.panels.engine.as_mut().expect("checked above");
        let title = self.project.title(Some(engine));
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        let dirty = self.project.dirty(engine);
        if ctx.input(|i| i.viewport().close_requested())
            && dirty
            && !self.quitting
            && self.smoke.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.project.confirm = Some(Action::Quit);
        }
        let Some(action) = self.project.confirm.clone() else {
            return;
        };
        let name = self.project.meta.name.clone();
        let can_save = self.project.dir.is_some();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("discard_changes")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(format!("“{name}” has changes that are not saved."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if can_save && ui.button("Save").clicked() {
                    choice = Some(true);
                }
                if ui.button("Discard changes").clicked() {
                    choice = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.project.confirm = None;
                }
            });
        });
        if modal.should_close() {
            self.project.confirm = None;
        }
        let Some(save) = choice else {
            return;
        };
        if save {
            self.save();
            if self
                .project
                .message
                .as_ref()
                .is_some_and(|(_, error)| *error)
            {
                self.project.confirm = None;
                return;
            }
        }
        if action == Action::Quit {
            self.project.confirm = None;
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if let Some(engine) = &mut self.panels.engine {
            let view = self.project.run(engine, action);
            self.show_loaded(view);
        }
    }

    /// The open project's kernel operators: hot reload of saved
    /// `kernels/*.atk` files (FR-K4). Parameter values whose name and kind
    /// still fit the new schema are kept.
    fn kernels_frame(&mut self) {
        let (Some(engine), Some(kernels)) = (&mut self.panels.engine, &mut self.project.kernels)
        else {
            return;
        };
        // The watcher thread read and compiled the files; this applies them.
        let Some(reload) = engine.extend(|r, l| kernels.poll(r, l)) else {
            return;
        };
        let (edits, notes) = atd_ops_pnt::project::stale_edits(engine.network(), engine.registry());
        let mut message = match reload.errors.first() {
            Some(e) => (e.to_string(), true),
            None => (format!("Reloaded {}", reload.files.join(", ")), false),
        };
        if !edits.is_empty() {
            match engine.edit("Reload kernels", edits) {
                Ok(()) => message.0 += &format!(" ({})", notes.join("; ")),
                Err(e) => message = (format!("Reloading kernels: {e}"), true),
            }
        }
        self.project.message = Some(message);
    }

    /// The open project's Python operators: hot reload of saved files
    /// (FR-K4), and asking before they run (docs/python-ops.md).
    fn python_frame(&mut self, ctx: &egui::Context) {
        let (Some(engine), Some(python)) = (&mut self.panels.engine, &mut self.project.python)
        else {
            return;
        };
        // The watcher thread read the files; this applies them (no disk).
        if let Some(reload) = engine.extend(|r, l| python.poll(r, l)) {
            let (edits, notes) = atd_py::ops::stale_edits(engine.network(), engine.registry());
            let mut message = match reload.errors.first() {
                Some(e) => (e.to_string(), true),
                None => (format!("Reloaded {}", reload.files.join(", ")), false),
            };
            if !edits.is_empty() {
                match engine.edit("Reload Python operators", edits) {
                    Ok(()) => message.0 += &format!(" ({})", notes.join("; ")),
                    Err(e) => message = (format!("Reloading Python operators: {e}"), true),
                }
            }
            self.project.message = Some(message);
        }
        if !self.project.python_ask || self.smoke.is_some() {
            return;
        }
        let Some(python) = &mut self.project.python else {
            return;
        };
        let files: Vec<String> = python
            .files()
            .map(|f| format!("{}/{}.py", atd_py::ops::OPS_DIR, f.stem))
            .collect();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("allow_python_ops")).show(ctx, |ui| {
            ui.heading("Run this project's Python operators?");
            ui.label(
                "This project contains operators written in Python. They are programs: once \
                 they run they can do anything your user account can (read and change files, \
                 use the network). Atidarla does not sandbox them. Allow them only if you \
                 trust where the project came from.",
            );
            ui.add_space(4.0);
            for f in &files {
                ui.monospace(f);
            }
            ui.add_space(4.0);
            ui.weak(
                "Until you allow them, their nodes do not cook. \"Remember\" applies to these \
                 files exactly as they are; any change asks again.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Allow and remember").clicked() {
                    choice = Some(true);
                }
                if ui.button("Allow this time").clicked() {
                    choice = Some(false);
                }
                if ui.button("Don't run").clicked() {
                    self.project.python_ask = false;
                }
            });
        });
        if modal.should_close() {
            self.project.python_ask = false;
        }
        let Some(remember) = choice else { return };
        self.project.python_ask = false;
        python.allow();
        if remember {
            let stored = atd_py::ops::TrustStore::user()
                .ok_or_else(|| "no settings folder".to_owned())
                .and_then(|s| {
                    s.remember(python.hash(), python.dir())
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = stored {
                self.project.message = Some((format!("Could not remember the choice: {e}"), true));
            }
        }
    }

    /// The textport's parameter changes, applied here as undoable edits,
    /// and its output (docs/textport.md). Never waits for Python.
    fn textport_frame(&mut self, ctx: &egui::Context) {
        let allowed = self.project.python.as_ref().is_some_and(|p| p.is_allowed());
        let p = &mut self.panels;
        let Some(engine) = &mut p.engine else {
            return;
        };
        let busy = p.textport.frame(
            engine,
            p.time,
            self.project.opened,
            self.project.dir.as_deref(),
            allowed,
        );
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn shortcuts(&mut self, ui: &Ui) {
        if self.panels.engine.is_none() || ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        // Longer shortcuts first: Cmd+Shift+S also matches Cmd+S.
        if ui.input_mut(|i| i.consume_shortcut(&SAVE_AS)) {
            self.project.save_as();
        } else if ui.input_mut(|i| i.consume_shortcut(&SAVE)) {
            self.save();
        } else if ui.input_mut(|i| i.consume_shortcut(&OPEN)) {
            self.request(Action::Open);
        } else if ui.input_mut(|i| i.consume_shortcut(&NEW)) {
            self.request(Action::New);
        }
        let Some(engine) = &mut self.panels.engine else {
            return;
        };
        let undo = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
        let redo = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        let redo_y = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
        // Check the longer shortcut first: Cmd+Shift+Z also matches Cmd+Z.
        if ui.input_mut(|i| i.consume_shortcut(&redo) || i.consume_shortcut(&redo_y)) {
            let _ = engine.redo();
        } else if ui.input_mut(|i| i.consume_shortcut(&undo)) {
            let _ = engine.undo();
        }
    }

    /// Cooks the viewed node and hands its texture to egui.
    /// Publishes the pointer (over the viewed image) and held keys for the
    /// Input Devices operator.
    fn feed_input(&mut self, ctx: &egui::Context) {
        let p = &mut self.panels;
        let (pos, buttons, scroll, keys) = ctx.input(|i| {
            let b = |btn| i.pointer.button_down(btn);
            (
                i.pointer.latest_pos(),
                [
                    b(egui::PointerButton::Primary),
                    b(egui::PointerButton::Middle),
                    b(egui::PointerButton::Secondary),
                ],
                i.smooth_scroll_delta.y,
                i.keys_down.iter().map(|k| k.name()).collect::<Vec<_>>(),
            )
        });
        let mut pointer = atd_io::input::pointer().unwrap_or_default();
        pointer.inside = false;
        if let (Some(pos), Some(rect)) = (pos, p.image_rect) {
            let uv = (pos - rect.min) / rect.size();
            pointer.x = uv.x.clamp(0.0, 1.0);
            pointer.y = (1.0f32 - uv.y).clamp(0.0, 1.0);
            pointer.inside = rect.contains(pos);
            if pointer.inside {
                // About one unit per wheel notch.
                p.wheel += scroll / 50.0;
            }
        }
        pointer.buttons = if pointer.inside { buttons } else { [false; 3] };
        pointer.wheel = p.wheel;
        atd_io::input::set_pointer(pointer);
        atd_io::input::set_keys(keys);
    }

    fn cook_and_present(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let p = &mut self.panels;
        let Some(engine) = &mut p.engine else { return };
        if let Some(sel) = p
            .patch
            .selected
            .filter(|s| engine.network().node(*s).is_some())
        {
            p.viewed = Some(sel);
        }
        if p.viewed.is_some_and(|v| engine.network().node(v).is_none()) {
            p.viewed = None;
        }
        let viewed = p.viewed;
        p.time = self.start.elapsed().as_secs_f64();
        let cooked = match p.stage.cook_frame(engine, viewed, p.time) {
            Ok(n) => n,
            Err(e) => {
                p.patch.status(e.to_string());
                0
            }
        };
        let Some(viewed) = viewed else {
            if cooked > 0 {
                ctx.request_repaint();
            }
            return;
        };
        if cooked > 0 {
            // Something changed or is animating: keep frames coming.
            ctx.request_repaint();
        }
        if let Some(ch) = engine.channels(viewed) {
            if cooked > 0 || p.history.is_empty() {
                p.history.push(viewed, ch);
            }
            return;
        }
        let (Some(rs), Some(tex)) = (frame.wgpu_render_state(), engine.result(viewed)) else {
            return;
        };
        if cooked > 0 || p.viewer.stale() {
            let mut renderer = rs.renderer.write();
            p.viewer.present(&rs.device, &rs.queue, &mut renderer, tex);
        }
    }
}

fn tab_title(tab: Tab) -> &'static str {
    match tab {
        Tab::Patch => "Patch",
        Tab::Stage => "Stage",
        Tab::Timeline => "Timeline",
        Tab::Parameters => "Parameters",
        Tab::Viewer => "Viewer",
        Tab::Operators => "Operators",
        Tab::Platform => "Platform",
        Tab::Textport => "Textport",
    }
}

fn platform_summary(p: &PlatformInfo) -> String {
    match &p.gpu_name {
        Some(gpu) => format!(
            "{} · {} · {}",
            p.os,
            gpu,
            p.gpu_backend.as_deref().unwrap_or("?")
        ),
        None => format!("{} · no GPU info", p.os),
    }
}

impl TabViewer for Panels {
    type Tab = Tab;

    fn title(&self, tab: &Tab) -> String {
        tab_title(*tab).to_owned()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Patch => match &mut self.engine {
                Some(engine) => {
                    self.canvas_rect = ui.available_rect_before_wrap();
                    self.patch.ui(ui, engine);
                }
                None => placeholder(ui, "No GPU device: the engine is unavailable."),
            },
            Tab::Stage => match &mut self.engine {
                Some(engine) => self.stage.ui(ui, engine, self.time),
                None => placeholder(ui, "No GPU device: the engine is unavailable."),
            },
            Tab::Timeline => placeholder(ui, "Keyframes, cues and timecode arrive in M3."),
            Tab::Parameters => match (&mut self.engine, self.patch.selected) {
                (Some(engine), Some(node)) => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(e) = crate::params::ui(ui, engine, node) {
                            self.patch.status(e);
                        }
                    });
                }
                _ => placeholder(ui, "Select a node to edit its parameters."),
            },
            Tab::Viewer => self.viewer_ui(ui),
            Tab::Operators => self.operators_ui(ui),
            Tab::Platform => self.platform_ui(ui),
            Tab::Textport => self.textport.ui(ui, self.engine.as_ref()),
        }
    }
}

impl Panels {
    fn viewer_ui(&mut self, ui: &mut Ui) {
        if let (Some(engine), Some(viewed)) = (&self.engine, self.viewed)
            && let Some(ch) = engine.channels(viewed)
        {
            let name = engine
                .network()
                .node(viewed)
                .map_or("?", |n| n.name.as_str());
            ui.horizontal(|ui| {
                ui.strong(name);
                ui.weak(match ch.len() {
                    1 => "1 channel".to_owned(),
                    n => format!("{n} channels"),
                });
            });
            self.history.ui(ui, ch);
            return;
        }
        let (Some(engine), Some(viewed), Some(tex)) =
            (&self.engine, self.viewed, self.viewer.texture_id)
        else {
            placeholder(ui, "Select a node to view its output.");
            return;
        };
        let name = engine
            .network()
            .node(viewed)
            .map_or("?", |n| n.name.as_str());
        // The viewed node's own size (operators may change resolution).
        let (w, h, format) = engine.result(viewed).map_or_else(
            || {
                let c = engine.config();
                (c.width, c.height, c.format)
            },
            |t| (t.width(), t.height(), t.format()),
        );
        ui.horizontal(|ui| {
            ui.strong(name);
            ui.weak(format!("{w}×{h} {format:?}"));
            if let Some(e) = engine.error(viewed) {
                ui.colored_label(egui::Color32::from_rgb(230, 80, 80), e);
            }
        });
        ui.horizontal(|ui| self.viewer.toolbar(ui));
        let avail = ui.available_size();
        let aspect = w as f32 / h.max(1) as f32;
        let size = if avail.x / avail.y.max(1.0) > aspect {
            egui::vec2(avail.y * aspect, avail.y)
        } else {
            egui::vec2(avail.x, avail.x / aspect)
        };
        ui.centered_and_justified(|ui| {
            let r = ui.add(
                egui::Image::new((tex, size))
                    .fit_to_exact_size(size)
                    .sense(egui::Sense::hover()),
            );
            self.image_rect = Some(r.rect);
            if let Some(p) = r.hover_pos() {
                let uv = (p - r.rect.min) / r.rect.size();
                let (x, y) = (
                    (uv.x * w as f32).floor() as i64,
                    (uv.y * h as f32).floor() as i64,
                );
                r.on_hover_text_at_pointer(format!(
                    "pixel {x}, {y}  ·  uv {:.3}, {:.3}",
                    uv.x, uv.y
                ));
            }
        });
    }

    fn operators_ui(&mut self, ui: &mut Ui) {
        let Some(engine) = &mut self.engine else {
            placeholder(ui, "No GPU device: the engine is unavailable.");
            return;
        };
        ui.weak("Click to add to the patch (or press Tab over the canvas).");
        let mut add = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (group, ops) in palette_groups(engine.registry()) {
                ui.collapsing(format!("{group} ({})", ops.len()), |ui| {
                    for op in ops {
                        let label = match Registry::availability(op, &self.platform) {
                            Availability::Native if group == PROJECT_GROUP => {
                                RichText::new(format!("{}  ({})", op.label, op.family))
                            }
                            Availability::Native => RichText::new(&op.label),
                            Availability::Fallback { flag, .. } => {
                                RichText::new(format!("{}  [{flag}]", op.label))
                            }
                        };
                        if ui
                            .selectable_label(false, label)
                            .on_hover_text(format!("{}\n{}", op.summary, op.type_id))
                            .clicked()
                        {
                            add = Some(op.type_id.clone());
                        }
                    }
                });
            }
        });
        if let Some(type_id) = add {
            let at = if self.canvas_rect.is_positive() {
                self.patch.centre(self.canvas_rect)
            } else {
                egui::pos2(100.0, 100.0)
            };
            self.patch.add_node(engine, &type_id, at);
        }
    }

    fn platform_ui(&self, ui: &mut Ui) {
        let p = &self.platform;
        egui::Grid::new("platform_grid")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label("OS");
                ui.label(format!("{} ({:?})", p.os, p.arch));
                ui.end_row();
                ui.label("GPU vendor");
                ui.label(p.gpu_vendor.to_string());
                ui.end_row();
                if let Some(a) = &self.adapter {
                    ui.label("Adapter");
                    ui.label(&a.name);
                    ui.end_row();
                    ui.label("Backend");
                    ui.label(format!("{} ({})", a.backend, a.device_type));
                    ui.end_row();
                    ui.label("Driver");
                    ui.label(format!("{} {}", a.driver, a.driver_info));
                    ui.end_row();
                }
                ui.label("SDKs");
                ui.label(if p.sdks.is_empty() {
                    "none detected".to_owned()
                } else {
                    p.sdks.iter().cloned().collect::<Vec<_>>().join(", ")
                });
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.strong("Capability flags on this machine");
        egui::Grid::new("flags_grid").num_columns(2).show(ui, |ui| {
            for s in SAMPLE_FLAGS {
                let flag: CapabilityFlag = s.parse().expect("sample flags are valid");
                ui.monospace(s);
                if flag.is_satisfied_by(p) {
                    ui.colored_label(ui.visuals().widgets.active.fg_stroke.color, "available");
                } else {
                    ui.weak("fallback");
                }
                ui.end_row();
            }
        });
    }
}

/// The palette group of operators the open project defines.
const PROJECT_GROUP: &str = "Project";

/// The operator palette's groups: built-in operators by family, then the
/// open project's own operators (Python, docs/python-ops.md) as "Project".
/// Empty groups are left out.
fn palette_groups(registry: &Registry) -> Vec<(String, Vec<&atd_core::registry::OperatorSchema>)> {
    let mut groups: Vec<(String, Vec<_>)> = OpFamily::ALL
        .iter()
        .map(|&family| {
            let ops = registry
                .family(family)
                .filter(|op| registry.provenance(&op.type_id).is_none())
                .collect();
            (family.to_string(), ops)
        })
        .collect();
    groups.push((
        PROJECT_GROUP.to_owned(),
        registry
            .iter()
            .filter(|op| registry.provenance(&op.type_id).is_some())
            .collect(),
    ));
    groups.retain(|(_, ops)| !ops.is_empty());
    groups
}

fn placeholder(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| ui.weak(text));
}

/// Imports ISF shaders from the directories in `ATIDARLA_ISF_PATH` (FR-K6)
/// as `img.isf_*` operators. Project-local `shaders/` folders follow the
/// project format (PRD A11).
fn load_isf(registry: &mut Registry, library: &mut OpLibrary) {
    let Some(path) = std::env::var_os("ATIDARLA_ISF_PATH") else {
        return;
    };
    for dir in std::env::split_paths(&path) {
        match atd_ops_img::isf::load_dir(&dir) {
            Ok((defs, failed)) => {
                let ids = atd_ops_img::isf::register(defs, registry, library);
                log::info!("ISF: {} operators from {}", ids.len(), dir.display());
                for (name, err) in failed {
                    log::warn!("ISF: {name} not imported: {err}");
                }
            }
            Err(e) => log::warn!("ISF: cannot read {}: {e}", dir.display()),
        }
    }
}

fn builtin_registry() -> Registry {
    let mut registry = Registry::default();
    atd_ops_img::register(&mut registry);
    atd_ops_sig::register(&mut registry);
    atd_ops_pnt::register(&mut registry);
    atd_ops_msh::register(&mut registry);
    atd_ops_mat::register(&mut registry);
    atd_ops_dat::register(&mut registry);
    atd_ops_cmp::register(&mut registry);
    atd_ops_ai::register(&mut registry);
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project's Python operators show under "Project", not with the
    /// built-in operators of their family.
    #[test]
    fn project_operators_have_their_own_palette_group() {
        let mut registry = builtin_registry();
        let builtin: Vec<String> = palette_groups(&registry)
            .into_iter()
            .map(|(g, _)| g)
            .collect();
        assert!(!builtin.contains(&PROJECT_GROUP.to_owned()));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/python-ops.atidarla");
        let mut python = atd_py::ops::PythonOps::open(&dir);
        python.register(&mut registry, &mut OpLibrary::default());
        let groups = palette_groups(&registry);
        let (name, project) = groups.last().unwrap();
        assert_eq!(name, PROJECT_GROUP);
        let ids: Vec<&str> = project.iter().map(|s| s.type_id.as_str()).collect();
        assert_eq!(
            ids,
            ["py.motion.counter", "py.motion.wobble", "py.pattern.rings"]
        );
        let sig = groups.iter().find(|(g, _)| g == "SIG").unwrap();
        assert!(sig.1.iter().all(|s| !s.type_id.starts_with("py.")));
        // The Tab search finds them too (it searches the whole registry).
        let hits = crate::patch::search(&registry, "rings");
        assert_eq!(hits[0].type_id, "py.pattern.rings");
    }
}
