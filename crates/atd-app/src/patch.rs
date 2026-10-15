//! The Patch editor (FR-P1): nodes, sockets and wires drawn over the canvas
//! and edited through the engine's undoable history.
//!
//! - Drag a node to move it; click to select it.
//! - Drag from an output socket (right side) to an input socket to wire.
//!   Right-click an input socket to disconnect it.
//! - Tab or double-click the background to search for an operator to add.
//! - Delete/Backspace removes the selected node.
//!
//! Drawing reads the engine immutably and queues edits, which are applied
//! after the frame's layout as undo steps.

use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Wire};
use atd_core::registry::{OpFamily, OperatorSchema, Registry, WireType};
use atd_gpu::engine::Engine;
use egui::epaint::CubicBezierShape;
use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use crate::canvas::{View, paint_grid};

pub const NODE_W: f32 = 150.0;
pub const HEADER: f32 = 26.0;
pub const ROW: f32 = 18.0;
const SOCKET_R: f32 = 5.0;
/// Screen distance within which a wire drop snaps to an input socket.
const SNAP: f32 = 14.0;
const STATUS_FOR: Duration = Duration::from_secs(5);

/// Node size in canvas units.
pub fn node_size(schema: &OperatorSchema) -> Vec2 {
    let rows = schema.inputs.len().max(schema.outputs.len()).max(1) as f32;
    Vec2::new(NODE_W, HEADER + rows * ROW + 6.0)
}

/// Canvas position of input socket `i` of a node at `pos`.
pub fn input_pos(pos: Pos2, i: usize) -> Pos2 {
    pos + Vec2::new(0.0, HEADER + ROW * (i as f32 + 0.5))
}

/// Canvas position of output socket `i` of a node at `pos`.
pub fn output_pos(pos: Pos2, i: usize) -> Pos2 {
    pos + Vec2::new(NODE_W, HEADER + ROW * (i as f32 + 0.5))
}

fn wire_colour(w: WireType) -> Color32 {
    match w {
        WireType::Img => Color32::from_rgb(180, 120, 255),
        WireType::Sig => Color32::from_rgb(110, 210, 140),
        WireType::Geo => Color32::from_rgb(90, 170, 255),
        WireType::Dat => Color32::from_rgb(230, 190, 90),
        WireType::Mat => Color32::from_rgb(240, 120, 120),
        WireType::Cmp => Color32::from_rgb(170, 170, 170),
    }
}

fn family_colour(f: OpFamily) -> Color32 {
    match f {
        OpFamily::Img => Color32::from_rgb(92, 64, 140),
        OpFamily::Sig => Color32::from_rgb(48, 110, 72),
        OpFamily::Pnt => Color32::from_rgb(40, 90, 140),
        OpFamily::Msh => Color32::from_rgb(40, 110, 120),
        OpFamily::Mat => Color32::from_rgb(130, 60, 60),
        OpFamily::Dat => Color32::from_rgb(120, 100, 40),
        OpFamily::Cmp => Color32::from_rgb(80, 80, 80),
        OpFamily::Ai => Color32::from_rgb(140, 70, 110),
    }
}

/// Matches operators against a search query: every word must appear in the
/// label, type ID or summary; label prefix matches rank first.
pub fn search<'a>(registry: &'a Registry, query: &str) -> Vec<&'a OperatorSchema> {
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<(u8, &OperatorSchema)> = registry
        .iter()
        .filter_map(|op| {
            let label = op.label.to_lowercase();
            let hay = format!("{label} {} {}", op.type_id, op.summary.to_lowercase());
            if !words.iter().all(|w| hay.contains(w)) {
                return None;
            }
            let rank = match words.first() {
                None => 1,
                Some(w) if label.starts_with(w) => 0,
                Some(w) if label.contains(w) => 1,
                _ => 2,
            };
            Some((rank, op))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.cmp(&b.1.label)));
    hits.into_iter().map(|(_, op)| op).collect()
}

#[derive(Debug)]
enum Drag {
    Node {
        id: NodeId,
        offset: Vec2,
    },
    Wire {
        from: NodeId,
        output: String,
        start: Pos2,
    },
}

#[derive(Debug)]
struct Search {
    query: String,
    /// Canvas position where the node will be placed.
    at: Pos2,
    /// Screen position of the popup.
    screen: Pos2,
    focus: bool,
}

#[derive(Debug, Default)]
pub struct PatchEditor {
    pub view: View,
    pub selected: Option<NodeId>,
    drag: Option<Drag>,
    search: Option<Search>,
    status: Option<(String, Instant)>,
}

impl PatchEditor {
    pub fn with_selection(selected: Option<NodeId>) -> Self {
        Self {
            selected,
            ..Default::default()
        }
    }

    /// Shows a transient message at the bottom of the canvas.
    pub fn status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// Adds a node of `type_id` at canvas position `at` and selects it.
    pub fn add_node(&mut self, engine: &mut Engine, type_id: &str, at: Pos2) {
        let short = type_id.split_once('.').map_or(type_id, |(_, n)| n);
        let id = engine.alloc_id();
        let mut node = Node::new(id, type_id, engine.network().unique_name(short));
        node.pos = [at.x.round(), at.y.round()];
        match engine.edit(&format!("add {short}"), [Edit::AddNode { node }]) {
            Ok(()) => self.selected = Some(id),
            Err(e) => self.status(e.to_string()),
        }
    }

    /// Canvas position at the centre of the last drawn canvas.
    pub fn centre(&self, rect: Rect) -> Pos2 {
        self.view.to_canvas(rect.min, rect.center())
    }

    pub fn ui(&mut self, ui: &mut Ui, engine: &mut Engine) {
        let (rect, bg) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let origin = rect.min;
        self.view.handle_input(ui, &bg, origin);
        let painter = ui.painter_at(rect);
        paint_grid(ui, &painter, rect, origin, self.view);
        let view = self.view;
        let zoom = view.zoom;
        let to_screen = |p: Pos2| view.to_screen(origin, p);
        let pointer = ui.ctx().pointer_latest_pos();

        let mut pending: Vec<(String, Vec<Edit>)> = Vec::new();
        let net = engine.network();
        let reg = engine.registry();

        // Snapshot of the node being dragged (the closure must not borrow self).
        let dragging = match &self.drag {
            Some(Drag::Node { id, offset }) => Some((*id, *offset)),
            _ => None,
        };
        let node_pos = |id: NodeId| -> Option<Pos2> {
            let n = net.node(id)?;
            let p = Pos2::new(n.pos[0], n.pos[1]);
            Some(match dragging {
                Some((d, offset)) if d == id => p + offset,
                _ => p,
            })
        };

        // Wires, under the nodes.
        for wire in net.wires() {
            let (Some(from), Some(to)) = (net.node(wire.from.node), net.node(wire.to.node)) else {
                continue;
            };
            let (Some(fs), Some(ts)) = (reg.get(&from.type_id), reg.get(&to.type_id)) else {
                continue;
            };
            let oi = fs
                .outputs
                .iter()
                .position(|s| s.name == wire.from.socket)
                .unwrap_or(0);
            let ii = ts
                .inputs
                .iter()
                .position(|s| s.name == wire.to.socket)
                .unwrap_or(0);
            let a = to_screen(output_pos(node_pos(from.id).unwrap_or_default(), oi));
            let b = to_screen(input_pos(node_pos(to.id).unwrap_or_default(), ii));
            let feedback = ts.inputs.get(ii).is_some_and(|s| s.feedback);
            let colour = wire_colour(fs.outputs.get(oi).map_or(WireType::Img, |s| s.wire));
            draw_wire(&painter, a, b, colour, zoom, feedback);
        }

        // Channel bindings: dashed lines from the SIG source to the bound
        // node's header (exports, FR-P2).
        for node in net.nodes() {
            for b in net.bindings_of(node.id) {
                let Some(src) = net.node(b.node) else {
                    continue;
                };
                let a = to_screen(output_pos(node_pos(src.id).unwrap_or_default(), 0));
                let t =
                    to_screen(node_pos(node.id).unwrap_or_default() + Vec2::new(0.0, HEADER * 0.5));
                let c = wire_colour(WireType::Sig).gamma_multiply(0.7);
                draw_wire(&painter, a, t, c, zoom, true);
            }
        }

        // Nodes and sockets.
        let mut input_targets: Vec<(Pos2, NodeId, String, WireType)> = Vec::new();
        for node in net.nodes() {
            let Some(schema) = reg.get(&node.type_id) else {
                continue;
            };
            let pos = node_pos(node.id).unwrap_or_default();
            let size = node_size(schema) * zoom;
            let screen = Rect::from_min_size(to_screen(pos), size);
            let resp = ui.interact(
                screen,
                Id::unique(("atd-node", node.id.0)),
                Sense::click_and_drag(),
            );
            if resp.clicked() || resp.drag_started() {
                self.selected = Some(node.id);
            }
            if resp.drag_started() {
                self.drag = Some(Drag::Node {
                    id: node.id,
                    offset: Vec2::ZERO,
                });
            }
            if resp.dragged()
                && let Some(Drag::Node { id, offset }) = &mut self.drag
                && *id == node.id
            {
                *offset += resp.drag_delta() / zoom;
            }
            if resp.drag_stopped()
                && let Some(Drag::Node { id, offset }) = self.drag.take()
                && id == node.id
                && offset != Vec2::ZERO
            {
                let p = Pos2::new(node.pos[0], node.pos[1]) + offset;
                pending.push((
                    format!("move {}", node.name),
                    vec![Edit::Move {
                        node: id,
                        pos: [p.x.round(), p.y.round()],
                    }],
                ));
            }

            let error = engine.error(node.id);
            let selected = self.selected == Some(node.id);
            let r = CornerRadius::same((6.0 * zoom).clamp(1.0, 12.0) as u8);
            painter.rect_filled(screen, r, ui.visuals().window_fill);
            let header = Rect::from_min_size(screen.min, Vec2::new(size.x, HEADER * zoom));
            painter.rect_filled(
                header,
                CornerRadius {
                    nw: r.nw,
                    ne: r.ne,
                    sw: 0,
                    se: 0,
                },
                family_colour(schema.family),
            );
            let stroke = match (error.is_some(), selected) {
                (true, _) => Stroke::new(2.0, Color32::from_rgb(230, 70, 70)),
                (false, true) => Stroke::new(2.0, ui.visuals().selection.stroke.color),
                _ => Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            };
            painter.rect_stroke(screen, r, stroke, StrokeKind::Outside);
            if zoom > 0.35 {
                painter.text(
                    header.left_center() + Vec2::new(8.0 * zoom, 0.0),
                    Align2::LEFT_CENTER,
                    &node.name,
                    FontId::proportional(13.0 * zoom),
                    Color32::WHITE,
                );
                painter.text(
                    screen.center_bottom() - Vec2::new(0.0, 5.0 * zoom),
                    Align2::CENTER_BOTTOM,
                    &schema.label,
                    FontId::proportional(10.0 * zoom),
                    ui.visuals().weak_text_color(),
                );
            }
            if let Some(e) = error {
                resp.on_hover_text(e);
            }

            for (i, s) in schema.inputs.iter().enumerate() {
                let c = to_screen(input_pos(pos, i));
                input_targets.push((c, node.id, s.name.clone(), s.wire));
                let hit = Rect::from_center_size(c, Vec2::splat(SNAP));
                let sr = ui.interact(hit, Id::unique(("atd-in", node.id.0, i)), Sense::click());
                if sr.secondary_clicked()
                    && let Some((_, src)) = net.inputs_of(node.id).find(|(n, _)| *n == s.name)
                {
                    let wire = Wire::new(src.clone(), Endpoint::new(node.id, s.name.clone()));
                    pending.push((
                        format!("disconnect {}.{}", node.name, s.name),
                        vec![Edit::Disconnect { wire }],
                    ));
                }
                let r = SOCKET_R * zoom.max(0.5);
                painter.circle_filled(c, r, wire_colour(s.wire));
                if s.feedback {
                    painter.circle_stroke(c, r + 2.0, Stroke::new(1.0, wire_colour(s.wire)));
                }
                if zoom > 0.6 {
                    painter.text(
                        c + Vec2::new(8.0 * zoom, 0.0),
                        Align2::LEFT_CENTER,
                        &s.name,
                        FontId::proportional(10.0 * zoom),
                        ui.visuals().text_color(),
                    );
                }
                sr.on_hover_text(format!(
                    "{} ({:?}){}",
                    s.name,
                    s.wire,
                    if s.feedback { ", feedback" } else { "" }
                ));
            }
            for (i, s) in schema.outputs.iter().enumerate() {
                let c = to_screen(output_pos(pos, i));
                let hit = Rect::from_center_size(c, Vec2::splat(SNAP));
                let sr = ui.interact(hit, Id::unique(("atd-out", node.id.0, i)), Sense::drag());
                if sr.drag_started() {
                    self.drag = Some(Drag::Wire {
                        from: node.id,
                        output: s.name.clone(),
                        start: c,
                    });
                }
                painter.circle_filled(c, SOCKET_R * zoom.max(0.5), wire_colour(s.wire));
                sr.on_hover_text(format!("{} ({:?}) — drag to an input", s.name, s.wire));
            }
        }

        // Wire being dragged.
        if let Some(Drag::Wire {
            from,
            output,
            start,
        }) = &self.drag
        {
            let released = ui.input(|i| i.pointer.any_released());
            if let Some(p) = pointer {
                let target = input_targets
                    .iter()
                    .filter(|t| t.0.distance(p) < SNAP)
                    .min_by(|a, b| a.0.distance(p).total_cmp(&b.0.distance(p)));
                let end = target.map_or(p, |t| t.0);
                draw_wire(
                    &painter,
                    *start,
                    end,
                    ui.visuals().selection.stroke.color,
                    zoom,
                    false,
                );
                if released {
                    if let Some((_, to, input, _)) = target {
                        let wire = Wire::new(
                            Endpoint::new(*from, output.clone()),
                            Endpoint::new(*to, input.clone()),
                        );
                        pending.push(("connect".into(), vec![Edit::Connect { wire }]));
                    }
                    self.drag = None;
                }
            } else if released {
                self.drag = None;
            }
        }

        if net.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Tab or double-click to add an operator",
                FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }
        painter.text(
            rect.left_bottom() + Vec2::new(8.0, -8.0),
            Align2::LEFT_BOTTOM,
            format!("{:.0}%", zoom * 100.0),
            FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
        if let Some((msg, at)) = &self.status {
            if at.elapsed() < STATUS_FOR {
                painter.text(
                    rect.center_bottom() - Vec2::new(0.0, 10.0),
                    Align2::CENTER_BOTTOM,
                    msg,
                    FontId::proportional(13.0),
                    Color32::from_rgb(240, 110, 110),
                );
                ui.ctx().request_repaint_after(Duration::from_millis(200));
            } else {
                self.status = None;
            }
        }

        if bg.clicked() {
            self.selected = None;
        }
        let tab = bg.hovered() && ui.input(|i| i.key_pressed(Key::Tab));
        if (bg.double_clicked() || tab)
            && let Some(p) = pointer
        {
            self.search = Some(Search {
                query: String::new(),
                at: view.to_canvas(origin, p),
                screen: p,
                focus: true,
            });
        }
        let typing = ui.ctx().egui_wants_keyboard_input();
        if !typing
            && self.search.is_none()
            && ui.rect_contains_pointer(rect)
            && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
            && let Some(id) = self.selected.take()
        {
            let name = net.node(id).map_or(String::new(), |n| n.name.clone());
            pending.push((format!("delete {name}"), vec![Edit::RemoveNode { id }]));
        }

        let chosen = self.search_ui(ui, reg);
        for (label, edits) in pending {
            if let Err(e) = engine.edit(&label, edits) {
                self.status(e.to_string());
            }
        }
        if let Some((type_id, at)) = chosen {
            self.add_node(engine, &type_id, at);
        }
    }

    /// The add-operator popup. Returns the chosen type and canvas position.
    fn search_ui(&mut self, ui: &mut Ui, reg: &Registry) -> Option<(String, Pos2)> {
        let state = self.search.as_mut()?;
        let mut chosen = None;
        let mut close = false;
        let area = egui::Area::new(Id::unique("atd-add-op"))
            .order(egui::Order::Foreground)
            .fixed_pos(state.screen)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(240.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut state.query).hint_text("Add operator…"),
                    );
                    if state.focus {
                        edit.request_focus();
                        state.focus = false;
                    }
                    let hits = search(reg, &state.query);
                    if hits.is_empty() {
                        ui.weak("No matching operators");
                    }
                    for op in hits.iter().take(12) {
                        let r = ui
                            .selectable_label(false, format!("{}  ·  {}", op.label, op.type_id))
                            .on_hover_text(&op.summary);
                        if r.clicked() {
                            chosen = Some(op.type_id.clone());
                        }
                    }
                    if ui.input(|i| i.key_pressed(Key::Enter))
                        && let Some(op) = hits.first()
                    {
                        chosen = Some(op.type_id.clone());
                    }
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                });
            });
        let clicked_elsewhere = ui.input(|i| i.pointer.any_pressed())
            && ui
                .ctx()
                .pointer_latest_pos()
                .is_some_and(|p| !area.response.rect.contains(p));
        let at = state.at;
        if chosen.is_some() || close || clicked_elsewhere {
            self.search = None;
        }
        chosen.map(|t| (t, at))
    }
}

fn draw_wire(painter: &egui::Painter, a: Pos2, b: Pos2, colour: Color32, zoom: f32, dashed: bool) {
    let dx = ((b.x - a.x).abs() * 0.5).max(40.0 * zoom);
    let points = [a, a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0), b];
    let width = (2.0 * zoom).clamp(1.0, 3.0);
    if dashed {
        let curve =
            CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, Stroke::NONE);
        let pts = curve.flatten(Some(0.5));
        painter.add(egui::Shape::dashed_line(
            &pts,
            Stroke::new(width, colour),
            6.0,
            4.0,
        ));
    } else {
        painter.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(width, colour),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_layout() {
        let p = Pos2::new(10.0, 20.0);
        assert_eq!(input_pos(p, 0), Pos2::new(10.0, 20.0 + HEADER + ROW * 0.5));
        assert_eq!(output_pos(p, 1).x, 10.0 + NODE_W);
    }

    #[test]
    fn search_ranks_label_prefix_first() {
        let mut reg = Registry::default();
        atd_ops_img::register(&mut reg);
        let hits = search(&reg, "co");
        assert_eq!(hits[0].type_id, "img.composite");
        assert!(hits.iter().any(|o| o.type_id == "img.constant"));
        assert!(
            search(&reg, "gaussian")
                .iter()
                .any(|o| o.type_id == "img.blur"),
            "matches summaries"
        );
        assert!(search(&reg, "zzzz").is_empty());
        assert_eq!(search(&reg, "").len(), reg.len());
    }
}
