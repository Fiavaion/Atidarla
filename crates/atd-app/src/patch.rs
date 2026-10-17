//! The Patch editor (FR-P1): nodes, sockets and wires drawn over the canvas
//! and edited through the engine's undoable history.
//!
//! - Click a node to select it; Shift/Ctrl/Cmd-click adds or removes it
//!   from the selection; Shift-drag the background to box-select; Ctrl+A
//!   selects all. Drag a node to move the selection.
//! - Drag from an output socket (right side) to an input socket to wire.
//!   Right-click an input socket to disconnect it.
//! - Tab or double-click the background to search for an operator to add.
//! - C wraps the selection in a comment box (or adds one at the pointer).
//!   Drag a comment's title bar to move it with the nodes inside, drag its
//!   corner to resize, double-click the title bar to edit the text.
//! - Delete/Backspace removes the selection; Ctrl+C/X/V copy, cut and paste
//!   it through the system clipboard (as a JSON fragment); Ctrl+D
//!   duplicates it. Each of these is one undo step.
//!
//! Drawing reads the engine immutably and queues edits, which are applied
//! after the frame's layout as undo steps.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use atd_core::fragment::Fragment;
use atd_core::graph::{Comment, CommentId, Edit, Endpoint, Node, NodeId, Wire};
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
/// Below this zoom, nodes and wires draw in a cheap, text-free form.
const LOD_ZOOM: f32 = 0.35;
/// Minimap size in screen points.
const MINIMAP: Vec2 = Vec2::new(180.0, 120.0);

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
            // Exact label, then the whole query as typed at the start of the
            // label, inside it, then word matches.
            let q = q.trim();
            let rank = match words.first() {
                None => 2,
                _ if label == q => 0,
                _ if label.starts_with(q) => 1,
                _ if label.contains(q) => 2,
                Some(w) if label.starts_with(w) => 3,
                Some(w) if label.contains(w) => 4,
                _ => 5,
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
    /// Box selection from `start` (screen).
    Select {
        start: Pos2,
    },
    /// Moving (with the nodes inside it) or resizing a comment box.
    Comment {
        id: CommentId,
        offset: Vec2,
        resize: bool,
        contained: BTreeSet<NodeId>,
    },
}

/// Text being edited in a comment box.
#[derive(Debug)]
struct CommentEdit {
    id: CommentId,
    text: String,
    /// Screen position of the editor.
    at: Pos2,
    focus: bool,
}

/// Height of a comment's title bar, in canvas units.
const COMMENT_HEADER: f32 = 26.0;
const MIN_COMMENT: Vec2 = Vec2::new(80.0, 40.0);
const COMMENT_FILL: Color32 = Color32::from_rgba_premultiplied(30, 36, 46, 70);

/// Canvas offset of pasted and duplicated nodes.
const PASTE_OFFSET: [f32; 2] = [30.0, 30.0];

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
    /// The active node: shown in the viewer and parameter panel.
    pub selected: Option<NodeId>,
    /// Every selected node (contains `selected`).
    pub selection: BTreeSet<NodeId>,
    drag: Option<Drag>,
    search: Option<Search>,
    status: Option<(String, Instant)>,
    selected_comment: Option<CommentId>,
    editing: Option<CommentEdit>,
    /// Frame the whole patch on the next draw (after opening a project).
    fit: bool,
}

impl PatchEditor {
    /// Frames the whole patch the next time the canvas is drawn.
    pub fn frame_all(mut self) -> Self {
        self.fit = true;
        self
    }

    /// Canvas bounds of every node and comment.
    fn bounds(engine: &Engine) -> Option<Rect> {
        let net = engine.network();
        let nodes = net.nodes().filter_map(|n| {
            let schema = engine.registry().get(&n.type_id)?;
            Some(Rect::from_min_size(
                Pos2::new(n.pos[0], n.pos[1]),
                node_size(schema),
            ))
        });
        let comments = net.comments().map(|c| {
            Rect::from_min_size(
                Pos2::new(c.pos[0], c.pos[1]),
                Vec2::new(c.size[0], c.size[1]),
            )
        });
        nodes.chain(comments).reduce(|a, b| a.union(b))
    }

    pub fn with_selection(selected: Option<NodeId>) -> Self {
        Self {
            selected,
            selection: selected.into_iter().collect(),
            ..Default::default()
        }
    }

    /// Selects only `id` (or nothing).
    pub fn select_only(&mut self, id: Option<NodeId>) {
        self.selected = id;
        self.selection = id.into_iter().collect();
        self.selected_comment = None;
    }

    /// Adds `id` to the selection or removes it.
    fn toggle(&mut self, id: NodeId) {
        if self.selection.remove(&id) {
            if self.selected == Some(id) {
                self.selected = self.selection.iter().next_back().copied();
            }
        } else {
            self.selection.insert(id);
            self.selected = Some(id);
        }
    }

    /// Edits that paste `fragment` moved by `offset`; selects the copies.
    fn paste_edits(
        &mut self,
        engine: &mut Engine,
        fragment: &Fragment,
        offset: [f32; 2],
    ) -> Vec<Edit> {
        let mut ids = Vec::with_capacity(fragment.nodes.len());
        for _ in &fragment.nodes {
            ids.push(engine.alloc_id());
        }
        let mut next = ids.iter().copied();
        let (edits, new_ids) = fragment.paste(
            engine.network(),
            || next.next().expect("one ID per node"),
            offset,
        );
        self.selection = new_ids.iter().copied().collect();
        self.selected = new_ids.last().copied();
        edits
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
            Ok(()) => self.select_only(Some(id)),
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
        let frame_key = bg.hovered()
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::F) && i.modifiers.is_none());
        if (self.fit || frame_key)
            && rect.width() > 0.0
            && let Some(bounds) = Self::bounds(engine)
        {
            self.view.fit(rect.size(), bounds);
            self.fit = false;
        }
        let shift = ui.input(|i| i.modifiers.shift);
        let boxing = matches!(self.drag, Some(Drag::Select { .. }));
        if !boxing && !(shift && bg.drag_started()) {
            self.view.handle_input(ui, &bg, origin);
        }
        // Forget nodes removed since the last frame (undo, deletes).
        {
            let net = engine.network();
            self.selection.retain(|id| net.node(*id).is_some());
            if self.selected.is_some_and(|id| net.node(id).is_none()) {
                self.selected = self.selection.iter().next_back().copied();
            }
        }
        let painter = ui.painter_at(rect);
        paint_grid(ui, &painter, rect, origin, self.view);
        let view = self.view;
        let zoom = view.zoom;
        let to_screen = |p: Pos2| view.to_screen(origin, p);
        let pointer = ui.ctx().pointer_latest_pos();

        let mut pending: Vec<(String, Vec<Edit>)> = Vec::new();
        let net = engine.network();
        let reg = engine.registry();

        // Snapshot of the nodes being dragged (the closure must not borrow
        // self): the whole selection when dragging a selected node.
        let (moving, drag_offset): (BTreeSet<NodeId>, Vec2) = match &self.drag {
            Some(Drag::Node { id, offset }) if self.selection.contains(id) => {
                (self.selection.clone(), *offset)
            }
            Some(Drag::Node { id, offset }) => (BTreeSet::from([*id]), *offset),
            Some(Drag::Comment {
                offset,
                resize: false,
                contained,
                ..
            }) => (contained.clone(), *offset),
            _ => (BTreeSet::new(), Vec2::ZERO),
        };
        let node_pos = |id: NodeId| -> Option<Pos2> {
            let n = net.node(id)?;
            let p = Pos2::new(n.pos[0], n.pos[1]);
            Some(if moving.contains(&id) {
                p + drag_offset
            } else {
                p
            })
        };
        let modifier = ui.input(|i| i.modifiers.shift || i.modifiers.command);

        // Comment boxes, under everything else.
        let comment_drag = match &self.drag {
            Some(Drag::Comment {
                id, offset, resize, ..
            }) => Some((*id, *offset, *resize)),
            _ => None,
        };
        for c in net.comments() {
            let mut pos = Pos2::new(c.pos[0], c.pos[1]);
            let mut size = Vec2::new(c.size[0], c.size[1]);
            if let Some((id, off, resize)) = comment_drag
                && id == c.id
            {
                if resize {
                    size = (size + off).max(MIN_COMMENT);
                } else {
                    pos += off;
                }
            }
            let screen = Rect::from_min_size(to_screen(pos), size * zoom);
            if !rect.intersects(screen) {
                continue;
            }
            let selected = self.selected_comment == Some(c.id);
            let stroke = if selected {
                Stroke::new(2.0, ui.visuals().selection.stroke.color)
            } else {
                Stroke::new(1.0, Color32::from_white_alpha(40))
            };
            let r = (6.0 * zoom).clamp(1.0, 10.0);
            painter.rect(screen, r, COMMENT_FILL, stroke, StrokeKind::Inside);
            let header = Rect::from_min_size(
                screen.min,
                Vec2::new(screen.width(), (COMMENT_HEADER * zoom).min(screen.height())),
            );
            painter.rect_filled(
                header,
                CornerRadius {
                    nw: r as u8,
                    ne: r as u8,
                    sw: 0,
                    se: 0,
                },
                Color32::from_white_alpha(14),
            );
            if zoom > 0.2 {
                let clip = painter.with_clip_rect(screen.shrink(2.0));
                let mut lines = c.text.lines();
                clip.text(
                    header.left_center() + Vec2::new(8.0 * zoom, 0.0),
                    Align2::LEFT_CENTER,
                    lines.next().unwrap_or(""),
                    FontId::proportional(14.0 * zoom),
                    Color32::from_gray(220),
                );
                let body: Vec<&str> = lines.collect();
                if !body.is_empty() && zoom > LOD_ZOOM {
                    clip.text(
                        header.left_bottom() + Vec2::new(8.0 * zoom, 6.0 * zoom),
                        Align2::LEFT_TOP,
                        body.join("\n"),
                        FontId::proportional(11.0 * zoom),
                        ui.visuals().weak_text_color(),
                    );
                }
            }
            let hr = ui.interact(
                header,
                Id::unique(("atd-comment", c.id.0)),
                Sense::click_and_drag(),
            );
            let grip = Rect::from_min_size(screen.max - Vec2::splat(14.0), Vec2::splat(14.0));
            let gr = ui.interact(
                grip,
                Id::unique(("atd-comment-size", c.id.0)),
                Sense::drag(),
            );
            for k in [4.0, 8.0] {
                painter.line_segment(
                    [
                        Pos2::new(screen.max.x - k, screen.max.y - 2.0),
                        Pos2::new(screen.max.x - 2.0, screen.max.y - k),
                    ],
                    Stroke::new(1.0, Color32::from_white_alpha(80)),
                );
            }
            if hr.clicked() || hr.drag_started() {
                self.selected_comment = Some(c.id);
                if !modifier {
                    self.selected = None;
                    self.selection.clear();
                }
            }
            if hr.double_clicked() {
                self.editing = Some(CommentEdit {
                    id: c.id,
                    text: c.text.clone(),
                    at: header.left_bottom(),
                    focus: true,
                });
            }
            if hr.drag_started() {
                // Nodes wholly inside the box move with it.
                let area = Rect::from_min_size(pos, size);
                let contained = net
                    .nodes()
                    .filter(|n| {
                        reg.get(&n.type_id).is_some_and(|s| {
                            area.contains_rect(Rect::from_min_size(
                                Pos2::new(n.pos[0], n.pos[1]),
                                node_size(s),
                            ))
                        })
                    })
                    .map(|n| n.id)
                    .collect();
                self.drag = Some(Drag::Comment {
                    id: c.id,
                    offset: Vec2::ZERO,
                    resize: false,
                    contained,
                });
            }
            if gr.drag_started() {
                self.drag = Some(Drag::Comment {
                    id: c.id,
                    offset: Vec2::ZERO,
                    resize: true,
                    contained: BTreeSet::new(),
                });
            }
            if (hr.dragged() || gr.dragged())
                && let Some(Drag::Comment { id, offset, .. }) = &mut self.drag
                && *id == c.id
            {
                *offset += (hr.drag_delta() + gr.drag_delta()) / zoom;
            }
            if (hr.drag_stopped() || gr.drag_stopped())
                && let Some(Drag::Comment {
                    id,
                    offset,
                    resize,
                    contained,
                }) = self.drag.take()
                && id == c.id
                && offset != Vec2::ZERO
            {
                let mut comment = c.clone();
                let mut edits = Vec::new();
                if resize {
                    let s = (Vec2::new(c.size[0], c.size[1]) + offset).max(MIN_COMMENT);
                    comment.size = [s.x.round(), s.y.round()];
                } else {
                    comment.pos = [(c.pos[0] + offset.x).round(), (c.pos[1] + offset.y).round()];
                    for n in contained.iter().filter_map(|id| net.node(*id)) {
                        edits.push(Edit::Move {
                            node: n.id,
                            pos: [(n.pos[0] + offset.x).round(), (n.pos[1] + offset.y).round()],
                        });
                    }
                }
                edits.push(Edit::UpdateComment { comment });
                let label = if resize {
                    "resize comment"
                } else {
                    "move comment"
                };
                pending.push((label.into(), edits));
            }
        }

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
            // Culling: off-screen nodes are neither drawn nor hit-tested
            // (sockets reach SNAP beyond the node's edge).
            if !rect.intersects(screen.expand(SNAP)) {
                continue;
            }
            let resp = ui.interact(
                screen,
                Id::unique(("atd-node", node.id.0)),
                Sense::click_and_drag(),
            );
            if resp.clicked() || resp.drag_started() {
                self.selected_comment = None;
            }
            if resp.clicked() {
                if modifier {
                    self.toggle(node.id);
                } else {
                    self.select_only(Some(node.id));
                }
            } else if resp.drag_started() {
                if !self.selection.contains(&node.id) {
                    if modifier {
                        self.selection.insert(node.id);
                    } else {
                        self.selection.clear();
                        self.selection.insert(node.id);
                    }
                }
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
                let moves: Vec<Edit> = moving
                    .iter()
                    .filter_map(|m| net.node(*m))
                    .map(|n| {
                        let p = Pos2::new(n.pos[0], n.pos[1]) + offset;
                        Edit::Move {
                            node: n.id,
                            pos: [p.x.round(), p.y.round()],
                        }
                    })
                    .collect();
                let label = if moves.len() == 1 {
                    format!("move {}", node.name)
                } else {
                    format!("move {} nodes", moves.len())
                };
                pending.push((label, moves));
            }

            let error = engine.error(node.id);
            let selected = self.selection.contains(&node.id);
            let active = self.selected == Some(node.id);
            if zoom < LOD_ZOOM {
                // Far out: one rect per node, no sockets to hit or draw
                // (zoom in to wire). Wire drops still find inputs.
                painter.rect_filled(screen, 1.0, family_colour(schema.family));
                if selected || error.is_some() {
                    let colour = if error.is_some() {
                        Color32::from_rgb(230, 70, 70)
                    } else {
                        ui.visuals().selection.stroke.color
                    };
                    painter.rect_stroke(screen, 1.0, Stroke::new(1.5, colour), StrokeKind::Outside);
                }
                for (i, s) in schema.inputs.iter().enumerate() {
                    input_targets.push((
                        to_screen(input_pos(pos, i)),
                        node.id,
                        s.name.clone(),
                        s.wire,
                    ));
                }
                continue;
            }
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
                (false, true) if active => Stroke::new(2.5, Color32::WHITE),
                (false, true) => Stroke::new(2.0, ui.visuals().selection.stroke.color),
                _ => Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            };
            painter.rect_stroke(screen, r, stroke, StrokeKind::Outside);
            {
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
            if let Some(e) = error
                && resp.hovered()
            {
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
                if sr.hovered() {
                    sr.on_hover_text(format!(
                        "{} ({:?}){}",
                        s.name,
                        s.wire,
                        if s.feedback { ", feedback" } else { "" }
                    ));
                }
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
                    // A SIG wire can also be dropped on a parameter to bind
                    // it to the node's first channel (FR-P2).
                    if s.wire == WireType::Sig {
                        egui::DragAndDrop::set_payload(
                            ui.ctx(),
                            atd_core::graph::ChannelRef {
                                node: node.id,
                                channel: String::new(),
                            },
                        );
                    }
                }
                painter.circle_filled(c, SOCKET_R * zoom.max(0.5), wire_colour(s.wire));
                if sr.hovered() {
                    sr.on_hover_text(format!("{} ({:?}) — drag to an input", s.name, s.wire));
                }
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
        self.minimap(ui, &painter, rect, origin, engine);
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

        // Box selection (Shift-drag on the background) adds to the selection.
        if bg.drag_started_by(egui::PointerButton::Primary)
            && shift
            && let Some(p) = pointer
        {
            self.drag = Some(Drag::Select { start: p });
        }
        if let Some(Drag::Select { start }) = &self.drag {
            let start = *start;
            if let Some(p) = pointer {
                let r = Rect::from_two_pos(start, p);
                painter.rect(
                    r,
                    2.0,
                    ui.visuals().selection.bg_fill.gamma_multiply(0.25),
                    Stroke::new(1.0, ui.visuals().selection.stroke.color),
                    StrokeKind::Inside,
                );
                if ui.input(|i| i.pointer.any_released()) {
                    for node in net.nodes() {
                        let Some(schema) = reg.get(&node.type_id) else {
                            continue;
                        };
                        let pos = Pos2::new(node.pos[0], node.pos[1]);
                        let screen = Rect::from_min_size(to_screen(pos), node_size(schema) * zoom);
                        if r.intersects(screen) {
                            self.selection.insert(node.id);
                            self.selected = Some(node.id);
                        }
                    }
                    self.drag = None;
                }
            } else {
                self.drag = None;
            }
        }

        if bg.clicked() && !modifier {
            self.select_only(None);
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
        let keys = !typing && self.search.is_none() && ui.rect_contains_pointer(rect);
        let mut clipboard: Option<Fragment> = None;
        let mut paste: Option<(Fragment, [f32; 2])> = None;
        let mut delete = false;
        let mut new_comment: Option<Rect> = None;
        if keys {
            let (select_all, duplicate, del, events, comment) = ui.input(|i| {
                (
                    i.modifiers.command && i.key_pressed(Key::A),
                    i.modifiers.command && i.key_pressed(Key::D),
                    i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                    i.events.clone(),
                    !i.modifiers.command && i.key_pressed(Key::C),
                )
            });
            if comment {
                new_comment =
                    Some(self.comment_area(net, reg, pointer.map(|p| view.to_canvas(origin, p))));
            }
            if select_all {
                self.selection = net.nodes().map(|n| n.id).collect();
                self.selected = self.selection.iter().next_back().copied();
            }
            delete = del;
            for event in events {
                match event {
                    egui::Event::Copy if !self.selection.is_empty() => {
                        clipboard = Some(Fragment::copy(net, &self.selection));
                    }
                    egui::Event::Cut if !self.selection.is_empty() => {
                        clipboard = Some(Fragment::copy(net, &self.selection));
                        delete = true;
                    }
                    egui::Event::Paste(text) => match Fragment::from_json(&text) {
                        Some(f) if !f.is_empty() => {
                            // Paste at the pointer, else next to the original.
                            let o = f.origin();
                            let offset = match pointer {
                                Some(p) => {
                                    let c = view.to_canvas(origin, p);
                                    [c.x.round() - o[0], c.y.round() - o[1]]
                                }
                                None => PASTE_OFFSET,
                            };
                            paste = Some((f, offset));
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            if duplicate && !self.selection.is_empty() {
                paste = Some((Fragment::copy(net, &self.selection), PASTE_OFFSET));
            }
        }
        if delete && let Some(id) = self.selected_comment.take() {
            pending.push(("delete comment".into(), vec![Edit::RemoveComment { id }]));
        }
        if delete && !self.selection.is_empty() {
            let removes: Vec<Edit> = self
                .selection
                .iter()
                .map(|id| Edit::RemoveNode { id: *id })
                .collect();
            let label = match removes.len() {
                1 => format!(
                    "delete {}",
                    self.selection
                        .first()
                        .and_then(|id| net.node(*id))
                        .map_or("", |n| n.name.as_str())
                ),
                n => format!("delete {n} nodes"),
            };
            pending.push((label, removes));
            self.select_only(None);
        }
        if let Some(f) = &clipboard {
            ui.ctx().copy_text(f.to_json());
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
        if let Some(area) = new_comment {
            let id = engine.alloc_comment_id();
            let comment = Comment {
                id,
                text: "Comment".into(),
                pos: [area.min.x.round(), area.min.y.round()],
                size: [area.width().round(), area.height().round()],
            };
            match engine.edit("add comment", [Edit::AddComment { comment }]) {
                Ok(()) => {
                    self.selected_comment = Some(id);
                    self.editing = Some(CommentEdit {
                        id,
                        text: "Comment".into(),
                        at: to_screen(area.min) + Vec2::new(0.0, COMMENT_HEADER * zoom),
                        focus: true,
                    });
                }
                Err(e) => self.status(e.to_string()),
            }
        }
        self.comment_editor(ui, engine);
        if let Some((fragment, offset)) = paste {
            let n = fragment.nodes.len();
            let edits = self.paste_edits(engine, &fragment, offset);
            if let Err(e) = engine.edit(&format!("paste {n} nodes"), edits) {
                self.select_only(None);
                self.status(e.to_string());
            }
        }
    }

    /// Canvas area for a new comment: around the selection, or at `at`.
    fn comment_area(
        &self,
        net: &atd_core::graph::Network,
        reg: &Registry,
        at: Option<Pos2>,
    ) -> Rect {
        let around = self
            .selection
            .iter()
            .filter_map(|id| net.node(*id))
            .filter_map(|n| {
                let s = reg.get(&n.type_id)?;
                Some(Rect::from_min_size(
                    Pos2::new(n.pos[0], n.pos[1]),
                    node_size(s),
                ))
            })
            .reduce(|a, b| a.union(b));
        match around {
            Some(r) => Rect::from_min_max(
                r.min - Vec2::new(24.0, 24.0 + COMMENT_HEADER),
                r.max + Vec2::splat(24.0),
            ),
            None => Rect::from_min_size(at.unwrap_or_default(), Vec2::new(320.0, 200.0)),
        }
    }

    /// The comment text editor popup; commits on Ctrl/Cmd+Enter or OK.
    fn comment_editor(&mut self, ui: &mut Ui, engine: &mut Engine) {
        let Some(edit) = &mut self.editing else {
            return;
        };
        let mut done: Option<bool> = None;
        egui::Area::new(Id::unique("atd-comment-editor"))
            .fixed_pos(edit.at)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(280.0);
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut edit.text)
                            .desired_rows(4)
                            .hint_text("First line is the title"),
                    );
                    if edit.focus {
                        r.request_focus();
                        edit.focus = false;
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked()
                            || ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter))
                        {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            done = Some(false);
                        }
                        ui.weak("Ctrl+Enter");
                    });
                });
            });
        match done {
            Some(true) => {
                let edit = self.editing.take().expect("editing");
                if let Some(old) = engine.network().comment(edit.id)
                    && old.text != edit.text
                {
                    let comment = Comment {
                        text: edit.text,
                        ..old.clone()
                    };
                    if let Err(e) = engine.edit("edit comment", [Edit::UpdateComment { comment }]) {
                        self.status(e.to_string());
                    }
                }
            }
            Some(false) => self.editing = None,
            None => {}
        }
    }

    /// Overview of the whole network in the bottom-right corner, with the
    /// visible area outlined; click or drag on it to move the view there.
    fn minimap(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        rect: Rect,
        origin: Pos2,
        engine: &Engine,
    ) {
        let net = engine.network();
        let reg = engine.registry();
        if net.is_empty() || rect.width() < 300.0 || rect.height() < 200.0 {
            return;
        }
        let visible = Rect::from_two_pos(
            self.view.to_canvas(origin, rect.min),
            self.view.to_canvas(origin, rect.max),
        );
        let mut bounds = visible;
        for node in net.nodes() {
            let size = reg.get(&node.type_id).map_or(Vec2::splat(40.0), node_size);
            let p = Pos2::new(node.pos[0], node.pos[1]);
            bounds = bounds.union(Rect::from_min_size(p, size));
        }
        for c in net.comments() {
            let r = Rect::from_min_size(
                Pos2::new(c.pos[0], c.pos[1]),
                Vec2::new(c.size[0], c.size[1]),
            );
            bounds = bounds.union(r);
        }
        let bounds = bounds.expand(40.0);
        let map = Rect::from_min_size(
            rect.right_bottom() - Vec2::new(MINIMAP.x + 10.0, MINIMAP.y + 10.0),
            MINIMAP,
        );
        let scale = (map.width() / bounds.width()).min(map.height() / bounds.height());
        let inner = Rect::from_center_size(map.center(), bounds.size() * scale);
        let to_map = |p: Pos2| inner.min + (p - bounds.min) * scale;

        painter.rect_filled(map, 4.0, ui.visuals().window_fill.gamma_multiply(0.85));
        painter.rect_stroke(
            map,
            4.0,
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
        for c in net.comments() {
            let r = Rect::from_two_pos(
                to_map(Pos2::new(c.pos[0], c.pos[1])),
                to_map(Pos2::new(c.pos[0] + c.size[0], c.pos[1] + c.size[1])),
            );
            painter.rect(
                r,
                1.0,
                COMMENT_FILL,
                Stroke::new(1.0, Color32::from_white_alpha(50)),
                StrokeKind::Inside,
            );
        }
        for node in net.nodes() {
            let Some(schema) = reg.get(&node.type_id) else {
                continue;
            };
            let p = to_map(Pos2::new(node.pos[0], node.pos[1]));
            let r = Rect::from_min_size(p, (node_size(schema) * scale).max(Vec2::splat(2.0)));
            let colour = if self.selection.contains(&node.id) {
                ui.visuals().selection.stroke.color
            } else {
                family_colour(schema.family)
            };
            painter.rect_filled(r, 0.0, colour);
        }
        painter.rect_stroke(
            Rect::from_two_pos(to_map(visible.min), to_map(visible.max)).intersect(map),
            0.0,
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Inside,
        );
        let resp = ui.interact(map, Id::unique("atd-minimap"), Sense::click_and_drag());
        if (resp.clicked() || resp.dragged())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let target = bounds.min + (p - inner.min) / scale;
            self.view.offset = rect.center() - origin - target.to_vec2() * self.view.zoom;
        }
        resp.on_hover_text("Overview: click or drag to move the view");
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
    // Culling: the curve stays inside its control points' bounding box.
    if !painter.clip_rect().intersects(Rect::from_points(&points)) {
        return;
    }
    if zoom < LOD_ZOOM {
        // Far out, curves are not worth tessellating.
        let colour = if dashed {
            colour.gamma_multiply(0.5)
        } else {
            colour
        };
        painter.line_segment([a, b], Stroke::new(1.0, colour));
        return;
    }
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
        // The whole query beats word-by-word matches.
        atd_ops_sig::register(&mut reg);
        assert_eq!(search(&reg, "audio in")[0].type_id, "sig.audio_in");
        assert_eq!(search(&reg, "").len(), reg.len());
    }

    /// FR-P1: a 2,000-node network stays interactive. Times layout plus
    /// tessellation of the whole Patch editor, headless, at 100% zoom (most
    /// nodes culled) and at 10% (every node visible).
    #[test]
    fn two_thousand_nodes_stay_interactive() {
        let instance = atd_gpu::primary_instance();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
        let Ok(adapter) = adapter else {
            let required =
                std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let mut library = atd_gpu::engine::OpLibrary::default();
        atd_ops_img::library(&mut library);
        let mut registry = Registry::default();
        atd_ops_img::register(&mut registry);
        atd_ops_sig::register(&mut registry);
        let config = atd_gpu::engine::EngineConfig {
            width: 64,
            height: 64,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut engine = Engine::new(device, queue, registry, library, config);

        // 40 rows of 50: noise → level → level … wired in a chain.
        let mut edits = Vec::new();
        for row in 0..40 {
            let mut prev = None;
            for col in 0..50 {
                let id = engine.alloc_id();
                let ty = if col == 0 { "img.noise" } else { "img.level" };
                let mut node = Node::new(id, ty, format!("n{row}_{col}"));
                node.pos = [col as f32 * 200.0, row as f32 * 90.0];
                edits.push(Edit::AddNode { node });
                if let Some(p) = prev {
                    edits.push(Edit::Connect {
                        wire: Wire::new(Endpoint::new(p, "out"), Endpoint::new(id, "in")),
                    });
                }
                prev = Some(id);
            }
        }
        engine.edit("build", edits).unwrap();
        assert_eq!(engine.network().len(), 2000);

        let mut editor = PatchEditor::default();
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))),
            ..Default::default()
        };
        for (label, zoom) in [("100%", 1.0), ("10%", 0.1)] {
            editor.view.zoom = zoom;
            let mut frame = || {
                let mut out = ctx.run_ui(input(), |ui| editor.ui(ui, &mut engine));
                out.textures_delta.clear();
                ctx.tessellate(out.shapes, out.pixels_per_point).len()
            };
            for _ in 0..3 {
                frame();
            }
            let frames = 10;
            let start = Instant::now();
            let mut prims = 0;
            for _ in 0..frames {
                prims = frame();
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);
            eprintln!("2,000 nodes at {label}: {ms:.2} ms/frame ({prims} primitives)");
            // Timing is only meaningful in optimised builds.
            if !cfg!(debug_assertions) {
                assert!(ms < 16.0, "{label}: {ms:.2} ms per frame");
            }
        }
    }
}
