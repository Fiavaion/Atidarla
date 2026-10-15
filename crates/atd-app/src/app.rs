//! Top-level application state and panel contents.

use atd_core::registry::{Availability, OpFamily};
use atd_core::{CapabilityFlag, PlatformInfo, Registry};
use egui::{RichText, Ui};

use crate::canvas::NodeCanvas;
use crate::dock::{DockState, TabViewer};

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
}

/// Everything the tabs render, split from the dock so both can be borrowed.
struct Panels {
    platform: PlatformInfo,
    adapter: Option<atd_gpu::AdapterReport>,
    registry: Registry,
    canvas: NodeCanvas,
}

impl AtidarlaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let info = cc
            .wgpu_render_state
            .as_ref()
            .map(|rs| rs.adapter.get_info());
        let platform = info
            .as_ref()
            .map_or_else(PlatformInfo::host, atd_gpu::platform_info);
        let adapter = info.as_ref().map(atd_gpu::AdapterReport::from_info);
        log::info!("platform: {platform:?}");

        let dock = DockState::new(
            vec![Tab::Operators, Tab::Platform],
            vec![Tab::Parameters, Tab::Viewer],
            vec![Tab::Timeline],
            vec![Tab::Patch, Tab::Stage],
        );
        Self {
            dock,
            panels: Panels {
                platform,
                adapter,
                registry: builtin_registry(),
                canvas: NodeCanvas::default(),
            },
        }
    }
}

impl std::fmt::Debug for AtidarlaApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtidarlaApp").finish_non_exhaustive()
    }
}

impl eframe::App for AtidarlaApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("View", |ui| {
                    for tab in [Tab::Patch, Tab::Stage, Tab::Timeline, Tab::Platform] {
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
            Tab::Patch => self.canvas.ui(ui),
            Tab::Stage => placeholder(ui, "Clip deck, layers and outputs arrive in M3."),
            Tab::Timeline => placeholder(ui, "Keyframes, cues and timecode arrive in M3."),
            Tab::Parameters => placeholder(ui, "Select a node to edit its parameters (M1)."),
            Tab::Viewer => placeholder(
                ui,
                "Texture, channel, geometry and table viewers arrive in M1–M2.",
            ),
            Tab::Operators => self.operators_ui(ui),
            Tab::Platform => self.platform_ui(ui),
        }
    }
}

impl Panels {
    fn operators_ui(&self, ui: &mut Ui) {
        if self.registry.is_empty() {
            ui.weak("No operators registered yet. The first IMG and SIG operators land in M1.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for family in OpFamily::ALL {
                let ops: Vec<_> = self.registry.family(family).collect();
                if ops.is_empty() {
                    continue;
                }
                ui.collapsing(format!("{family} ({})", ops.len()), |ui| {
                    for op in ops {
                        match Registry::availability(op, &self.platform) {
                            Availability::Native => {
                                ui.label(&op.label);
                            }
                            Availability::Fallback { flag, fallback } => {
                                ui.horizontal(|ui| {
                                    ui.label(&op.label);
                                    ui.label(RichText::new(flag.to_string()).small().strong())
                                        .on_hover_text(fallback.explanation());
                                });
                            }
                        }
                    }
                });
            }
        });
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
