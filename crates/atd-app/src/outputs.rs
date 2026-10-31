//! Outputs (M3, FR-S6, decision A54): a native window per enabled
//! `cmp.window` node, showing the node's mapped image stretched over the
//! window, and the Outputs tab to edit a window's settings and slices:
//! source rectangles, warps (control points dragged over a preview),
//! soft edges, masks and test patterns. Every change is an edit of the
//! node's parameters (`slices` is rewritten in its canonical text), so it
//! is undoable and saved; the mapping itself happens in the node's cook.
//!
//! 10-bit/HDR: eframe gives every window the same 8-bit surface format
//! (`egui_wgpu::preferred_framebuffer_format`), so outputs are 8-bit for
//! now (`docs/outputs.md`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use atd_core::graph::{Edit, NodeId, Param};
use atd_gpu::engine::Engine;
use atd_gpu::output::{MAX_GRID, MAX_MASK_POINTS, OutputMap, Pattern, Slice, Warp};
use atd_ops_cmp::window::TYPE_ID;
use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::viewer::{Mode, TextureViewer};

/// Handle radius in the preview, in points.
const HANDLE: f32 = 5.0;
const SELECTED: Color32 = Color32::from_rgb(255, 200, 40);
const OTHER: Color32 = Color32::from_rgb(120, 160, 220);
const MASK: Color32 = Color32::from_rgb(240, 90, 160);

#[derive(Debug)]
struct Output {
    /// The node's image as a display texture (RGB: projectors show no alpha).
    viewer: TextureViewer,
    /// What the window's own pass reads and reports.
    shared: Arc<Mutex<Shared>>,
}

/// State shared with a window's (deferred) viewport callback.
#[derive(Debug, Default)]
struct Shared {
    texture: Option<egui::TextureId>,
    /// The size in pixels of the display under the window, once known.
    display_px: Option<[u32; 2]>,
    /// The window was asked to close.
    close: bool,
}

impl Default for Output {
    fn default() -> Self {
        let mut viewer = TextureViewer::default();
        viewer.mode = Mode::Rgb;
        Self {
            viewer,
            shared: Default::default(),
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Default)]
pub struct OutputsPanel {
    /// Window nodes cooked every frame: the enabled ones and the one the
    /// tab shows.
    cook: Vec<NodeId>,
    /// Enabled window nodes (each has a native window).
    open: Vec<NodeId>,
    outputs: HashMap<NodeId, Output>,
    selected: Option<NodeId>,
    slice: usize,
    /// Clicks and drags in the preview edit the mask instead of the warp.
    edit_mask: bool,
    /// The point being dragged in the preview.
    drag: Option<usize>,
    message: Option<String>,
}

fn param<'a>(engine: &'a Engine, node: NodeId, name: &str) -> Option<&'a Value> {
    engine.network().param_value(engine.registry(), node, name)
}

fn windows(engine: &Engine) -> impl Iterator<Item = NodeId> + '_ {
    engine
        .network()
        .nodes()
        .filter(|n| n.type_id == TYPE_ID)
        .map(|n| n.id)
}

impl OutputsPanel {
    /// Collects this frame's window nodes; returns those to cook every
    /// frame along with the viewed node.
    pub fn prepare(&mut self, engine: &Engine) -> &[NodeId] {
        self.open.clear();
        self.cook.clear();
        for id in windows(engine) {
            let enabled = param(engine, id, "enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if enabled {
                self.open.push(id);
            }
            if enabled || self.selected == Some(id) {
                self.cook.push(id);
            }
        }
        let cook = &self.cook;
        self.outputs.retain(|id, _| cook.contains(id));
        &self.cook
    }

    /// Draws each cooked window's image into its display texture.
    pub fn present(&mut self, engine: &Engine, rs: &eframe::egui_wgpu::RenderState, cooked: usize) {
        for &id in &self.cook {
            let Some(tex) = engine.result(id) else {
                continue;
            };
            let out = self.outputs.entry(id).or_default();
            if cooked > 0 || out.viewer.stale() {
                let mut renderer = rs.renderer.write();
                out.viewer
                    .present(&rs.device, &rs.queue, &mut renderer, tex);
                lock(&out.shared).texture = out.viewer.texture_id;
            }
        }
    }

    /// Shows a native window per enabled window node, repainted when
    /// something cooked. Each is a deferred viewport: it paints in its own
    /// pass, so a window never waits for the main window's UI. Closing one
    /// turns its node's Enabled off (an undoable edit).
    pub fn show_windows(&mut self, ctx: &egui::Context, engine: &mut Engine, cooked: usize) {
        let mut closed = Vec::new();
        for &id in &self.open {
            let out = self.outputs.entry(id).or_default();
            if std::mem::take(&mut lock(&out.shared).close) {
                closed.push(id);
                continue;
            }
            let text = |name| {
                param(engine, id, name)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            // As the node reads it: live, else bound or expression-driven
            // (PRD A66), else stored.
            let vec2p = |name, d: [f32; 2]| {
                let f = |i: usize| engine.param_component(id, name, i, true).map(|v| v as f32);
                [f(0).unwrap_or(d[0]), f(1).unwrap_or(d[1])]
            };
            let (fullscreen, display) = screen_mode(engine, id);
            let size = vec2p("window_size", [960.0, 540.0]);
            let pos = vec2p("position", [80.0, 80.0]);
            let mut builder = egui::ViewportBuilder::default()
                .with_title(text("title"))
                .with_inner_size([size[0].max(64.0), size[1].max(36.0)])
                .with_position(pos)
                .with_fullscreen(fullscreen);
            if fullscreen {
                builder = builder.with_monitor(display);
            }
            let viewport = egui::ViewportId::from_hash_of(("atd-output", id));
            let shared = out.shared.clone();
            ctx.show_viewport_deferred(viewport, builder, move |ui, _class| {
                let texture = lock(&shared).texture;
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(Color32::BLACK))
                    .show(ui, |ui| {
                        if let Some(tex) = texture {
                            ui.painter().image(
                                tex,
                                ui.max_rect(),
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                    });
                let (close, px) = ui.input(|i| {
                    let v = i.viewport();
                    let px = v
                        .monitor_size
                        .zip(v.native_pixels_per_point)
                        .map(|(s, p)| [(s.x * p).round() as u32, (s.y * p).round() as u32]);
                    (v.close_requested(), px)
                });
                let mut s = lock(&shared);
                s.close |= close;
                if px.is_some() {
                    s.display_px = px;
                }
            });
            if cooked > 0 {
                ctx.request_repaint_of(viewport);
            }
        }
        for id in closed {
            let edit = Edit::SetParam {
                node: id,
                param: "enabled".into(),
                value: Some(Param::constant(json!(false))),
            };
            if let Err(e) = engine.edit("Close output window", [edit]) {
                self.message = Some(e.to_string());
            }
        }
    }

    fn set(&mut self, engine: &mut Engine, node: NodeId, name: &str, value: Value) {
        let label = engine
            .network()
            .node(node)
            .map_or_else(|| name.to_owned(), |n| format!("{}.{name}", n.name));
        let edit = Edit::SetParam {
            node,
            param: name.into(),
            value: Some(Param::constant(value)),
        };
        if let Err(e) = engine.edit_coalesced(&label, &format!("{node}:{name}"), [edit]) {
            self.message = Some(e.to_string());
        }
    }

    /// The Outputs tab.
    pub fn ui(&mut self, ui: &mut Ui, engine: &mut Engine) {
        let nodes: Vec<(NodeId, String)> = windows(engine)
            .map(|id| {
                let name = engine.network().node(id).map_or("?", |n| n.name.as_str());
                (id, name.to_owned())
            })
            .collect();
        if nodes.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak(
                    "No outputs yet. Add a Window (cmp.window) in the Patch and wire your \
                     composition into it: it opens a window you can put on a projector.",
                )
            });
            return;
        }
        if !self
            .selected
            .is_some_and(|s| nodes.iter().any(|n| n.0 == s))
        {
            self.selected = Some(nodes[0].0);
            self.slice = 0;
        }
        ui.horizontal_wrapped(|ui| {
            for (id, name) in &nodes {
                let on = param(engine, *id, "enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let label = if on {
                    name.clone()
                } else {
                    format!("{name} (off)")
                };
                if ui
                    .selectable_label(self.selected == Some(*id), label)
                    .on_hover_text(engine.status(*id).unwrap_or(""))
                    .clicked()
                    && self.selected != Some(*id)
                {
                    self.selected = Some(*id);
                    self.slice = 0;
                    self.drag = None;
                }
            }
        });
        if let Some(msg) = &self.message {
            ui.colored_label(ui.visuals().error_fg_color, msg);
        }
        let Some(node) = self.selected else { return };
        ui.separator();
        self.settings(ui, engine, node);
        ui.separator();

        let text = param(engine, node, "slices")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut map = match OutputMap::parse(&text) {
            Ok(map) => map,
            Err(e) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Slices: {e}. Fix the text in the Parameters panel."),
                );
                return;
            }
        };
        self.slice = self.slice.min(map.slices.len() - 1);
        let mut changed = self.slices_ui(ui, &mut map);
        changed |= self.preview(ui, engine, node, &mut map);
        if changed {
            self.set(engine, node, "slices", json!(map.to_text()));
        }
        if !ui.input(|i| i.pointer.any_down()) {
            engine.seal_history();
        }
    }

    /// Window settings: enabled, fullscreen, display, pattern, resolution.
    fn settings(&mut self, ui: &mut Ui, engine: &mut Engine, node: NodeId) {
        let b = |name| {
            param(engine, node, name)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let i = |name| {
            param(engine, node, name)
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let (mut enabled, mut fullscreen) = (b("enabled"), b("fullscreen"));
        let (mut display, mut width, mut height) = (i("display"), i("width"), i("height"));
        let mut pattern = param(engine, node, "pattern")
            .and_then(Value::as_str)
            .unwrap_or("off")
            .to_owned();
        let display_px = self
            .outputs
            .get(&node)
            .and_then(|o| lock(&o.shared).display_px);
        let mut edits: Vec<(&str, Value)> = Vec::new();
        ui.horizontal_wrapped(|ui| {
            if ui
                .checkbox(&mut enabled, "Window")
                .on_hover_text("Open this output's window")
                .changed()
            {
                edits.push(("enabled", json!(enabled)));
            }
            if ui
                .checkbox(&mut fullscreen, "Fullscreen")
                .on_hover_text("Borderless fullscreen on the display below")
                .changed()
            {
                edits.push(("fullscreen", json!(fullscreen)));
            }
            ui.label("Display");
            if ui
                .add(egui::DragValue::new(&mut display).range(0..=15))
                .on_hover_text("The display a fullscreen window opens on, in the system's order")
                .changed()
            {
                edits.push(("display", json!(display)));
            }
            ui.separator();
            egui::ComboBox::from_id_salt(("output-pattern", node))
                .selected_text(format!("Pattern: {pattern}"))
                .show_ui(ui, |ui| {
                    for name in Pattern::NAMES {
                        if ui
                            .selectable_value(&mut pattern, name.to_owned(), name)
                            .changed()
                        {
                            edits.push(("pattern", json!(name)));
                        }
                    }
                });
            ui.separator();
            ui.label("Resolution");
            if ui
                .add(egui::DragValue::new(&mut width).range(0..=8192))
                .changed()
            {
                edits.push(("width", json!(width)));
            }
            ui.label("×");
            if ui
                .add(egui::DragValue::new(&mut height).range(0..=8192))
                .changed()
            {
                edits.push(("height", json!(height)));
            }
            if let Some([w, h]) = display_px
                && ui
                    .button(format!("Match display ({w}×{h})"))
                    .on_hover_text(
                        "Set the resolution to the pixels of the display the window is on",
                    )
                    .clicked()
            {
                edits.push(("width", json!(w)));
                edits.push(("height", json!(h)));
            }
        });
        for (name, value) in edits {
            self.set(engine, node, name, value);
        }
    }

    /// The slice list and the selected slice's settings; true when the map
    /// changed.
    fn slices_ui(&mut self, ui: &mut Ui, map: &mut OutputMap) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Slice");
            egui::ComboBox::from_id_salt("output-slice")
                .selected_text(&map.slices[self.slice].name)
                .show_ui(ui, |ui| {
                    for (k, s) in map.slices.iter().enumerate() {
                        ui.selectable_value(&mut self.slice, k, &s.name);
                    }
                });
            if map.slices.len() < atd_gpu::output::MAX_SLICES && ui.button("Add").clicked() {
                let mut n = map.slices.len() + 1;
                while map.slices.iter().any(|s| s.name == n.to_string()) {
                    n += 1;
                }
                map.slices.push(Slice::new(&n.to_string()));
                self.slice = map.slices.len() - 1;
                changed = true;
            }
            if map.slices.len() > 1 && ui.button("Remove").clicked() {
                map.slices.remove(self.slice);
                self.slice = self.slice.min(map.slices.len() - 1);
                changed = true;
            }
        });
        let s = &mut map.slices[self.slice];
        let drag = |ui: &mut Ui, v: &mut f32, label: &str, max: f32| {
            ui.add(
                egui::DragValue::new(v)
                    .speed(0.002)
                    .range(0.0..=max)
                    .fixed_decimals(3)
                    .prefix(label),
            )
            .changed()
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Source");
            for (v, l) in s.source.iter_mut().zip(["x ", "y ", "w ", "h "]) {
                changed |= drag(ui, v, l, 1.0);
            }
            ui.separator();
            let mut warp = s.warp;
            egui::ComboBox::from_id_salt("output-warp")
                .selected_text(format!("Warp: {}", warp.name()))
                .show_ui(ui, |ui| {
                    for name in Warp::NAMES {
                        let w = Warp::from_name(name).expect("listed");
                        ui.selectable_value(&mut warp, w, name);
                    }
                });
            if warp != s.warp {
                set_warp(s, warp);
                changed = true;
            }
            let [mut cols, mut rows] = s.grid;
            ui.label("Grid");
            let fixed = s.warp == Warp::Perspective;
            let step = if s.warp == Warp::Bezier { 3.0 } else { 1.0 };
            let r1 = ui.add_enabled(
                !fixed,
                egui::DragValue::new(&mut cols)
                    .range(2..=MAX_GRID)
                    .speed(0.05),
            );
            ui.label("×");
            let r2 = ui.add_enabled(
                !fixed,
                egui::DragValue::new(&mut rows)
                    .range(2..=MAX_GRID)
                    .speed(0.05),
            );
            if (r1.changed() || r2.changed()) && [cols, rows] != s.grid {
                let fit = |n: u32| {
                    if step == 3.0 { bezier_count(n) } else { n }
                };
                regrid(s, fit(cols), fit(rows));
                changed = true;
            }
            if s.points.is_some()
                && ui
                    .button("Reset warp")
                    .on_hover_text("Back to the destination rectangle")
                    .clicked()
            {
                s.points = None;
                changed = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Soft edge");
            for (v, l) in s.blend.iter_mut().zip(["L ", "R ", "T ", "B "]) {
                changed |= drag(ui, v, l, 1.0);
            }
            changed |= ui
                .add(
                    egui::DragValue::new(&mut s.blend_power)
                        .speed(0.02)
                        .range(0.1..=16.0)
                        .prefix("power "),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut s.blend_gamma)
                        .speed(0.02)
                        .range(0.1..=8.0)
                        .prefix("gamma "),
                )
                .changed();
            ui.separator();
            ui.toggle_value(&mut self.edit_mask, "Edit mask").on_hover_text(
                "Drag the mask's points in the preview; click to add one, right-click to remove one",
            );
            if self.edit_mask && s.mask.is_empty() && ui.button("New mask").clicked() {
                s.mask = bounds(&s.control_points());
                changed = true;
            }
            if !s.mask.is_empty() {
                changed |= ui.checkbox(&mut s.mask_invert, "Invert").changed();
                if ui.button("Clear mask").clicked() {
                    s.mask.clear();
                    changed = true;
                }
            }
        });
        changed
    }

    /// The node's image with the slices' control points (and masks) drawn
    /// over it; dragging a point of the selected slice moves it. True when
    /// the map changed.
    fn preview(&mut self, ui: &mut Ui, engine: &Engine, node: NodeId, map: &mut OutputMap) -> bool {
        let (w, h) = engine
            .result(node)
            .map_or((16.0, 9.0), |t| (t.width() as f32, t.height() as f32));
        let avail = ui.available_size();
        let aspect = w / h.max(1.0);
        let size = if avail.x / avail.y.max(1.0) > aspect {
            vec2(avail.y * aspect, avail.y)
        } else {
            vec2(avail.x, avail.x / aspect)
        };
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        if let Some(tex) = self.outputs.get(&node).and_then(|o| o.viewer.texture_id) {
            painter.image(
                tex,
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let to_screen = |p: [f32; 2]| rect.min + vec2(p[0] * rect.width(), p[1] * rect.height());
        let from_screen = |q: Pos2| {
            [
                ((q.x - rect.min.x) / rect.width()).clamp(-0.5, 1.5),
                ((q.y - rect.min.y) / rect.height()).clamp(-0.5, 1.5),
            ]
        };
        for (k, s) in map.slices.iter().enumerate() {
            let colour = if k == self.slice { SELECTED } else { OTHER };
            let pts = s.control_points();
            let [cols, rows] = [s.grid[0] as usize, s.grid[1] as usize];
            let stroke = Stroke::new(if k == self.slice { 1.5 } else { 1.0 }, colour);
            for j in 0..rows {
                for i in 0..cols {
                    let p = to_screen(pts[j * cols + i]);
                    if i + 1 < cols {
                        painter.line_segment([p, to_screen(pts[j * cols + i + 1])], stroke);
                    }
                    if j + 1 < rows {
                        painter.line_segment([p, to_screen(pts[(j + 1) * cols + i])], stroke);
                    }
                    if k == self.slice && !self.edit_mask {
                        painter.circle_filled(p, HANDLE, colour);
                    }
                }
            }
            if !s.mask.is_empty() {
                let mut ring: Vec<Pos2> = s.mask.iter().map(|p| to_screen(*p)).collect();
                ring.push(ring[0]);
                painter.line(ring, Stroke::new(1.5, MASK));
                if k == self.slice && self.edit_mask {
                    for p in &s.mask {
                        painter.circle_filled(to_screen(*p), HANDLE, MASK);
                    }
                }
            }
        }

        let s = &mut map.slices[self.slice];
        let nearest = |pts: &[[f32; 2]], q: Pos2| {
            pts.iter()
                .enumerate()
                .map(|(i, p)| (i, to_screen(*p).distance(q)))
                .filter(|(_, d)| *d <= HANDLE * 2.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let mut changed = false;
        if resp.drag_started()
            && let Some(q) = resp.interact_pointer_pos()
        {
            self.drag = if self.edit_mask {
                nearest(&s.mask, q)
            } else {
                nearest(&s.control_points(), q)
            };
        }
        if resp.dragged()
            && let (Some(i), Some(q)) = (self.drag, resp.interact_pointer_pos())
        {
            let p = from_screen(q);
            if self.edit_mask {
                if let Some(m) = s.mask.get_mut(i) {
                    *m = p;
                    changed = true;
                }
            } else {
                let mut pts = s.control_points();
                if let Some(c) = pts.get_mut(i) {
                    *c = p;
                    s.points = Some(pts);
                    changed = true;
                }
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        if self.edit_mask
            && let Some(q) = resp.interact_pointer_pos()
        {
            if resp.clicked() && nearest(&s.mask, q).is_none() && s.mask.len() < MAX_MASK_POINTS {
                if s.mask.is_empty() {
                    s.mask = bounds(&s.control_points());
                } else {
                    s.mask.push(from_screen(q));
                }
                changed = true;
            }
            if resp.secondary_clicked()
                && s.mask.len() > 3
                && let Some(i) = nearest(&s.mask, q)
            {
                s.mask.remove(i);
                changed = true;
            }
        }
        changed
    }
}

/// The smallest valid Bezier point count (3n + 1) at or above `n`.
fn bezier_count(n: u32) -> u32 {
    (n.max(2) - 1).div_ceil(3).clamp(1, (MAX_GRID - 1) / 3) * 3 + 1
}

/// Changes the control grid to `cols × rows`, keeping the slice's shape:
/// the new points are sampled from the current warp.
fn regrid(s: &mut Slice, cols: u32, rows: u32) {
    let old = s.control_points();
    let mut pts = Vec::with_capacity((cols * rows) as usize);
    for j in 0..rows {
        for i in 0..cols {
            pts.push(s.eval(
                &old,
                i as f32 / (cols - 1) as f32,
                j as f32 / (rows - 1) as f32,
            ));
        }
    }
    s.grid = [cols, rows];
    s.points = Some(pts);
}

/// Switches the warp, resampling the control grid to one the new warp
/// takes (2×2 for a corner pin, 3n+1 per axis for Bezier).
fn set_warp(s: &mut Slice, warp: Warp) {
    let [c, r] = s.grid;
    let grid = match warp {
        Warp::Perspective => [2, 2],
        Warp::Bezier => [bezier_count(c), bezier_count(r)],
        _ => s.grid,
    };
    if grid != s.grid {
        regrid(s, grid[0], grid[1]);
    }
    s.warp = warp;
}

/// A window node's Fullscreen toggle and Display index as the node reads
/// them: live, else bound or expression-driven, else stored.
fn screen_mode(engine: &Engine, id: NodeId) -> (bool, usize) {
    let fullscreen = engine
        .param_number(id, "fullscreen", true)
        .is_some_and(|v| v >= 0.5);
    let display = engine
        .param_number(id, "display", true)
        .map_or(0, |v| v.round().clamp(0.0, 15.0) as usize);
    (fullscreen, display)
}

/// The bounding rectangle of `pts` as a four-point polygon.
fn bounds(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in pts {
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warp_switches_keep_valid_grids() {
        assert_eq!(bezier_count(2), 4);
        assert_eq!(bezier_count(4), 4);
        assert_eq!(bezier_count(5), 7);
        assert_eq!(bezier_count(15), 16);
        assert_eq!(bezier_count(16), 16);
        let mut s = Slice::new("a");
        s.grid = [3, 3];
        set_warp(&mut s, Warp::Bezier);
        assert_eq!(s.grid, [4, 4]);
        set_warp(&mut s, Warp::Perspective);
        assert_eq!(s.grid, [2, 2]);
        // Whatever the panel does, the text it writes parses back.
        let map = OutputMap { slices: vec![s] };
        assert_eq!(OutputMap::parse(&map.to_text()).unwrap(), map);
    }

    /// Fullscreen and Display follow driven values, not only stored ones.
    #[test]
    fn screen_mode_reads_driven_values() {
        use serde_json::json;
        let Some(mut e) = crate::project::tests::engine() else {
            return;
        };
        let id = e.alloc_id();
        e.edit(
            "add",
            [Edit::AddNode {
                node: atd_core::graph::Node::new(id, atd_ops_cmp::window::TYPE_ID, "window1"),
            }],
        )
        .unwrap();
        assert_eq!(screen_mode(&e, id), (false, 0));
        e.set_live(id, "fullscreen", Some(&json!(true)));
        e.set_live(id, "display", Some(&json!(2)));
        assert_eq!(screen_mode(&e, id), (true, 2));
    }

    #[test]
    fn regridding_keeps_the_shape() {
        let mut s = Slice::new("a");
        s.points = Some(vec![[0.1, 0.0], [0.9, 0.1], [0.0, 1.0], [1.0, 0.8]]);
        let before = s.eval(&s.control_points(), 0.5, 0.5);
        regrid(&mut s, 3, 3);
        let p = s.control_points();
        assert_eq!(p.len(), 9);
        assert!((p[4][0] - before[0]).abs() < 1e-6 && (p[4][1] - before[1]).abs() < 1e-6);
    }
}
