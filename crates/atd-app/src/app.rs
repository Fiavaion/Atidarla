//! Top-level application state and panel contents.

use std::time::Instant;

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::{Availability, OpFamily};
use atd_core::{CapabilityFlag, PlatformInfo, Registry};
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use egui::{Key, KeyboardShortcut, Modifiers, RichText, Ui};
use serde_json::json;

use crate::dock::{DockState, TabViewer};
use crate::patch::PatchEditor;
use crate::smoke::{Smoke, SmokeArgs};

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
}

pub struct AtidarlaApp {
    dock: DockState<Tab>,
    panels: Panels,
    smoke: Option<Smoke>,
    start: Instant,
}

/// Everything the tabs render, split from the dock so both can be borrowed.
struct Panels {
    platform: PlatformInfo,
    adapter: Option<atd_gpu::AdapterReport>,
    engine: Option<Engine>,
    patch: PatchEditor,
    /// Node shown in the viewer: the selection, or the last selected node.
    viewed: Option<NodeId>,
    viewer_tex: Option<egui::TextureId>,
    canvas_rect: egui::Rect,
}

impl AtidarlaApp {
    pub fn new(cc: &eframe::CreationContext<'_>, smoke: Option<SmokeArgs>) -> Self {
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

        // The engine shares eframe's device, so the viewer shows engine
        // textures directly.
        let mut engine = rs.map(|rs| {
            let mut library = OpLibrary::default();
            atd_ops_img::library(&mut library);
            Engine::new(
                rs.device.clone(),
                rs.queue.clone(),
                builtin_registry(),
                library,
                EngineConfig {
                    width: 1280,
                    height: 720,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                },
            )
        });
        let viewed = engine.as_mut().and_then(demo_patch);

        let dock = DockState::new(
            vec![Tab::Operators, Tab::Platform],
            vec![Tab::Viewer, Tab::Parameters],
            vec![Tab::Timeline],
            vec![Tab::Patch, Tab::Stage],
        );
        Self {
            dock,
            panels: Panels {
                platform,
                adapter,
                engine,
                patch: PatchEditor::with_selection(viewed),
                viewed,
                viewer_tex: None,
                canvas_rect: egui::Rect::NOTHING,
            },
            smoke: smoke.map(Smoke::new),
            start: Instant::now(),
        }
    }
}

/// A small starting network so a new user sees the engine working:
/// Noise → Kaleidoscope → Level. Returns the node to view.
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
        wire(noise, kal),
        wire(kal, lvl),
        set(noise, "scale", json!(3.0)),
        set(noise, "monochrome", json!(false)),
        set(kal, "segments", json!(8)),
        set(lvl, "contrast", json!(1.3)),
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
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
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
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("Atidarla {}", atd_core::VERSION));
                    ui.separator();
                    ui.weak(platform_summary(&self.panels.platform));
                });
            });
        });
        self.dock.show(ui, &mut self.panels);
        self.cook_and_present(ui.ctx(), frame);
        if let Some(smoke) = &mut self.smoke {
            smoke.tick(ui.ctx());
        }
    }
}

impl AtidarlaApp {
    fn shortcuts(&mut self, ui: &Ui) {
        let Some(engine) = &mut self.panels.engine else {
            return;
        };
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
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
        let Some(viewed) = p.viewed else { return };
        let cooked = match engine.cook(&[viewed], self.start.elapsed().as_secs_f64()) {
            Ok(n) => n,
            Err(e) => {
                p.patch.status(e.to_string());
                0
            }
        };
        let (Some(rs), Some(tex)) = (frame.wgpu_render_state(), engine.result(viewed)) else {
            return;
        };
        if cooked > 0 || p.viewer_tex.is_none() {
            let view = tex.create_view(&Default::default());
            let mut renderer = rs.renderer.write();
            match p.viewer_tex {
                Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                    &rs.device,
                    &view,
                    wgpu::FilterMode::Linear,
                    id,
                ),
                None => {
                    p.viewer_tex = Some(renderer.register_native_texture(
                        &rs.device,
                        &view,
                        wgpu::FilterMode::Linear,
                    ))
                }
            }
            // Show the new result on the next frame.
            ctx.request_repaint();
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
            Tab::Stage => placeholder(ui, "Clip deck, layers and outputs arrive in M3."),
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
        }
    }
}

impl Panels {
    fn viewer_ui(&mut self, ui: &mut Ui) {
        let (Some(engine), Some(viewed), Some(tex)) = (&self.engine, self.viewed, self.viewer_tex)
        else {
            placeholder(ui, "Select a node to view its output.");
            return;
        };
        let name = engine
            .network()
            .node(viewed)
            .map_or("?", |n| n.name.as_str());
        let cfg = engine.config();
        ui.horizontal(|ui| {
            ui.strong(name);
            ui.weak(format!("{}×{} {:?}", cfg.width, cfg.height, cfg.format));
            if let Some(e) = engine.error(viewed) {
                ui.colored_label(egui::Color32::from_rgb(230, 80, 80), e);
            }
        });
        let avail = ui.available_size();
        let aspect = cfg.width as f32 / cfg.height as f32;
        let size = if avail.x / avail.y.max(1.0) > aspect {
            egui::vec2(avail.y * aspect, avail.y)
        } else {
            egui::vec2(avail.x, avail.x / aspect)
        };
        ui.centered_and_justified(|ui| {
            ui.add(egui::Image::new((tex, size)).fit_to_exact_size(size));
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
            for family in OpFamily::ALL {
                let ops: Vec<_> = engine.registry().family(family).collect();
                if ops.is_empty() {
                    continue;
                }
                ui.collapsing(format!("{family} ({})", ops.len()), |ui| {
                    for op in ops {
                        let label = match Registry::availability(op, &self.platform) {
                            Availability::Native => RichText::new(&op.label),
                            Availability::Fallback { flag, .. } => {
                                RichText::new(format!("{}  [{flag}]", op.label))
                            }
                        };
                        if ui
                            .selectable_label(false, label)
                            .on_hover_text(&op.summary)
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

fn placeholder(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| ui.weak(text));
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
