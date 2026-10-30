//! The Diff view (FR-F2, docs/merge.md): the open project against a folder
//! or a Git revision. Its canvas shows the patch with added nodes green
//! (`+`, solid outline), removed nodes red and faded (`−`, dashed
//! outline), changed nodes amber (`~`, thick outline) and moved-only nodes
//! (`↔`); the sign and outline carry the meaning as well as the colour.
//! Loading runs on a worker thread (rule 1).

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_core::graph::{Network, NodeId};
use atd_format::diff::{NodeChange, NodeDiff, Presence, ProjectDiff, endpoint_text, param_text};
use egui::{
    Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

use crate::canvas::{self, View};
use crate::merge::Side;
use crate::patch::{HEADER, NODE_W};

const ADDED: Color32 = Color32::from_rgb(60, 170, 90);
const REMOVED: Color32 = Color32::from_rgb(220, 70, 70);
const CHANGED: Color32 = Color32::from_rgb(235, 165, 40);

/// How a node is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Same,
    Added,
    Removed,
    Changed,
    Moved,
}

impl Mark {
    fn of(d: Option<&NodeDiff>) -> Self {
        match d {
            None => Self::Same,
            Some(d) if d.is_move_only() => Self::Moved,
            Some(d) => match d.presence {
                Presence::Added => Self::Added,
                Presence::Removed => Self::Removed,
                Presence::Changed => Self::Changed,
            },
        }
    }

    /// The sign drawn before the name, so the state reads without colour.
    pub fn sign(self) -> &'static str {
        match self {
            Self::Same => "",
            Self::Added => "+ ",
            Self::Removed => "− ",
            Self::Changed => "~ ",
            Self::Moved => "↔ ",
        }
    }
}

/// A finished comparison.
#[derive(Debug)]
pub struct Compared {
    pub label: String,
    pub diff: ProjectDiff,
    /// The main network of the other side and of the open project.
    pub old: Network,
    pub new: Network,
    pub selected: Option<NodeId>,
}

impl Compared {
    fn compute(label: String, old: atd_format::Project, new: atd_format::Project) -> Self {
        let diff = atd_format::diff::diff(&old, &new);
        let main = |p: &atd_format::Project| p.main().cloned().unwrap_or_default();
        Self {
            label,
            old: main(&old),
            new: main(&new),
            diff,
            selected: None,
        }
    }

    /// Every node to draw (the open project's, then the removed ones),
    /// with its mark.
    pub fn marks(&self) -> BTreeMap<NodeId, Mark> {
        let diffs: BTreeMap<NodeId, &NodeDiff> = self
            .diff
            .network(atd_format::MAIN)
            .map(|n| n.nodes.iter().map(|d| (d.id, d)).collect())
            .unwrap_or_default();
        self.new
            .nodes()
            .chain(self.old.nodes().filter(|n| self.new.node(n.id).is_none()))
            .map(|n| (n.id, Mark::of(diffs.get(&n.id).copied())))
            .collect()
    }

    fn node_diff(&self, id: NodeId) -> Option<&NodeDiff> {
        self.diff.network(atd_format::MAIN)?.node(id)
    }
}

type Job = Receiver<Result<Compared, String>>;

/// The Diff window.
#[derive(Debug, Default)]
pub struct DiffView {
    pub open: bool,
    /// The other side: a folder or `git:<rev>`.
    pub other: String,
    job: Option<Job>,
    pub compared: Option<Compared>,
    pub status: Option<(String, bool)>,
    view: View,
    fitted: bool,
}

impl DiffView {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Compares `new` (the open project) against `old` on a worker.
    pub fn start(&mut self, old: Side, new: Side) {
        let label = format!("{} → the open project", side_label(&old));
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = old
                .load()
                .and_then(|o| Ok(Compared::compute(label, o, new.load()?)));
            let _ = tx.send(result);
        });
        self.job = Some(rx);
        self.status = Some(("Comparing…".into(), false));
    }

    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the comparison stopped unexpectedly".into()),
        };
        self.job = None;
        match result {
            Ok(c) => {
                self.status = c.diff.is_empty().then(|| ("No differences.".into(), false));
                self.compared = Some(c);
                self.fitted = false;
            }
            Err(e) => self.status = Some((e, true)),
        }
        true
    }

    pub fn window(
        &mut self,
        ctx: &egui::Context,
        engine: Option<&atd_gpu::engine::Engine>,
        state: &crate::project::ProjectState,
    ) {
        if self.poll() {
            ctx.request_repaint();
        }
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Diff")
            .open(&mut open)
            .default_size([900.0, 620.0])
            .show(ctx, |ui| self.ui(ui, engine, state));
        self.open = open;
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        engine: Option<&atd_gpu::engine::Engine>,
        state: &crate::project::ProjectState,
    ) {
        ui.horizontal(|ui| {
            ui.label("Compare with");
            ui.add(
                egui::TextEdit::singleline(&mut self.other)
                    .hint_text("folder or git:<revision>, e.g. git:HEAD")
                    .desired_width(360.0),
            );
            if ui
                .add_enabled(
                    !self.busy() && engine.is_some(),
                    egui::Button::new("Compare"),
                )
                .clicked()
                && let Some(engine) = engine
            {
                let sides = Side::parse(&self.other, state.dir.as_deref())
                    .and_then(|old| Ok((old, crate::merge::open_side(engine, state)?)));
                match sides {
                    Ok((old, new)) => self.start(old, new),
                    Err(e) => self.status = Some((e, true)),
                }
            }
        });
        if let Some((msg, error)) = &self.status {
            if *error {
                ui.colored_label(REMOVED, format!("Error: {msg}"));
            } else {
                ui.label(msg.as_str());
            }
        }
        let Some(c) = &mut self.compared else {
            return;
        };
        ui.weak(&c.label);
        ui.horizontal_wrapped(|ui| {
            for (mark, colour, text) in [
                (Mark::Added, ADDED, "added"),
                (Mark::Removed, REMOVED, "removed"),
                (Mark::Changed, CHANGED, "changed"),
                (Mark::Moved, CHANGED, "moved"),
            ] {
                ui.label(RichText::new(format!("{}{text}", mark.sign())).color(colour));
            }
        });
        let fitted = &mut self.fitted;
        let view = &mut self.view;
        canvas_ui(ui, c, view, fitted);
        ui.columns(2, |cols| {
            change_list(&mut cols[0], c);
            node_table(&mut cols[1], c);
        });
    }
}

fn side_label(s: &Side) -> String {
    match s {
        Side::Open(_) => "the open project".into(),
        Side::Source(s) => s.to_string(),
    }
}

fn colour(mark: Mark) -> Color32 {
    match mark {
        Mark::Same => Color32::from_gray(140),
        Mark::Added => ADDED,
        Mark::Removed => REMOVED,
        Mark::Changed | Mark::Moved => CHANGED,
    }
}

/// The read-only canvas: wires, then nodes coloured and signed by mark.
fn canvas_ui(ui: &mut Ui, c: &mut Compared, view: &mut View, fitted: &mut bool) {
    let size = Vec2::new(ui.available_width(), 340.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let origin = rect.min;
    let marks = c.marks();
    let pos = |id: NodeId| {
        c.new
            .node(id)
            .or_else(|| c.old.node(id))
            .map_or(Pos2::ZERO, |n| Pos2::new(n.pos[0], n.pos[1]))
    };
    if !*fitted && !marks.is_empty() {
        let mut bounds = Rect::NOTHING;
        for id in marks.keys() {
            bounds = bounds.union(Rect::from_min_size(pos(*id), Vec2::new(NODE_W, HEADER)));
        }
        view.fit(rect.size(), bounds);
        *fitted = true;
    }
    view.handle_input(ui, &resp, origin);
    let painter = ui.painter_at(rect);
    canvas::paint_grid(ui, &painter, rect, origin, *view);
    let zoom = view.zoom;
    let screen = |id: NodeId| {
        Rect::from_min_size(
            view.to_screen(origin, pos(id)),
            Vec2::new(NODE_W, HEADER) * zoom,
        )
    };
    // Wires: the open project's solid, removed ones dashed red.
    let new_wires: Vec<_> = c.new.wires().collect();
    for w in &new_wires {
        let (a, b) = (
            screen(w.from.node).right_center(),
            screen(w.to.node).left_center(),
        );
        painter.line_segment([a, b], Stroke::new(1.5, Color32::from_gray(150)));
    }
    for w in c.old.wires().filter(|w| !new_wires.contains(w)) {
        let (a, b) = (
            screen(w.from.node).right_center(),
            screen(w.to.node).left_center(),
        );
        painter.extend(Shape::dashed_line(
            &[a, b],
            Stroke::new(1.5, REMOVED),
            6.0,
            4.0,
        ));
    }
    let mut hit = None;
    for (&id, &mark) in &marks {
        let r = screen(id);
        let col = colour(mark);
        let fill = match mark {
            Mark::Removed => ui.visuals().window_fill.gamma_multiply(0.4),
            _ => ui.visuals().window_fill,
        };
        painter.rect_filled(r, 3.0, fill);
        match mark {
            Mark::Removed => {
                let pts = [
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.left_top(),
                ];
                painter.extend(Shape::dashed_line(&pts, Stroke::new(1.5, col), 5.0, 3.0));
            }
            Mark::Changed => {
                painter.rect_stroke(r, 3.0, Stroke::new(3.0, col), StrokeKind::Outside);
            }
            _ => {
                painter.rect_stroke(r, 3.0, Stroke::new(1.5, col), StrokeKind::Outside);
            }
        }
        if c.selected == Some(id) {
            painter.rect_stroke(
                r.expand(3.0),
                4.0,
                Stroke::new(1.0, ui.visuals().strong_text_color()),
                StrokeKind::Outside,
            );
        }
        let name = c
            .new
            .node(id)
            .or_else(|| c.old.node(id))
            .map_or("", |n| &n.name);
        let text_col = if mark == Mark::Removed {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().text_color()
        };
        painter.text(
            r.left_center() + Vec2::new(6.0 * zoom, 0.0),
            Align2::LEFT_CENTER,
            format!("{}{name}", mark.sign()),
            FontId::proportional(13.0 * zoom),
            text_col,
        );
        if resp.clicked() && resp.interact_pointer_pos().is_some_and(|p| r.contains(p)) {
            hit = Some(id);
        }
    }
    if resp.clicked() {
        c.selected = hit;
    }
}

/// Every change as text: the accessible form of the canvas.
fn change_list(ui: &mut Ui, c: &mut Compared) {
    ui.label(RichText::new("Changes").strong());
    egui::ScrollArea::vertical()
        .id_salt("atd_diff_list")
        .max_height(200.0)
        .show(ui, |ui| {
            if c.diff.is_empty() {
                ui.weak("No differences.");
            }
            for s in &c.diff.settings {
                ui.label(format!("~ {}: {} → {}", s.key, s.old, s.new));
            }
            let mut select = None;
            for net in &c.diff.networks {
                ui.label(format!("{} network {}", net.presence.sign(), net.id));
                for d in &net.nodes {
                    let mark = Mark::of(Some(d));
                    let text = RichText::new(format!(
                        "  {}{} {} ({})",
                        mark.sign(),
                        d.id,
                        d.name,
                        d.type_id
                    ))
                    .color(colour(mark));
                    let main = net.id == atd_format::MAIN;
                    if ui
                        .selectable_label(main && c.selected == Some(d.id), text)
                        .clicked()
                        && main
                    {
                        select = Some(d.id);
                    }
                }
                for cm in &net.comments {
                    ui.label(format!("  {} comment {}", cm.presence.sign(), cm.id));
                }
            }
            for s in &c.diff.sections {
                ui.label(format!(
                    "{} {}: {}",
                    s.presence.sign(),
                    s.section,
                    s.summary
                ));
            }
            if select.is_some() {
                c.selected = select;
            }
        });
}

/// The selected node's changes: what, before, after.
fn node_table(ui: &mut Ui, c: &Compared) {
    let Some(id) = c.selected else {
        ui.weak("Click a node to see its changes.");
        return;
    };
    let Some(d) = c.node_diff(id) else {
        ui.weak("This node is unchanged.");
        return;
    };
    ui.label(
        RichText::new(format!(
            "{} {} ({}), {}",
            d.id,
            d.name,
            d.type_id,
            d.presence.label()
        ))
        .strong(),
    );
    egui::ScrollArea::vertical()
        .id_salt("atd_diff_table")
        .max_height(200.0)
        .show(ui, |ui| {
            egui::Grid::new("atd_diff_node")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Change");
                    ui.strong("Before");
                    ui.strong("After");
                    ui.end_row();
                    for ch in &d.changes {
                        let (what, old, new) = row(ch);
                        ui.label(what);
                        ui.monospace(old);
                        ui.monospace(new);
                        ui.end_row();
                    }
                });
        });
}

fn row(ch: &NodeChange) -> (String, String, String) {
    let p = |v: [f32; 2]| format!("[{:?}, {:?}]", v[0], v[1]);
    match ch {
        NodeChange::Renamed { old, new } => ("name".into(), old.clone(), new.clone()),
        NodeChange::Retyped { old, new } => ("type".into(), old.clone(), new.clone()),
        NodeChange::Moved { old, new } => ("position".into(), p(*old), p(*new)),
        NodeChange::Param { name, old, new } => (
            format!("param {name}"),
            param_text(old.as_ref()),
            param_text(new.as_ref()),
        ),
        NodeChange::Input { socket, old, new } => (
            format!("input {socket}"),
            endpoint_text(old.as_ref()),
            endpoint_text(new.as_ref()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_format::Project;

    const NET: &str = r#"format = 1
next_id = 3

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
"#;

    #[test]
    fn the_diff_view_marks_nodes_and_lists_changes() {
        let dir = std::env::temp_dir().join(format!("atd-app-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let old = Project::with_main("show", atd_format::network_from_str(NET).unwrap());
        atd_format::save(&dir, &old).unwrap();
        let new_text = NET
            .replace(
                "pos = [200.0, 0.0]",
                "pos = [200.0, 0.0]\nparams.gamma = 2.0",
            )
            .replace("next_id = 3", "next_id = 4")
            + "\n[[node]]\nid = 2\nname = \"blur1\"\ntype = \"img.blur\"\npos = [0.0, 100.0]\n";
        let new_text = new_text.replace(
            "[[node]]\nid = 0\nname = \"noise1\"\ntype = \"img.noise\"\npos = [0.0, 0.0]\n\n",
            "",
        );
        let new_text = new_text.replace("inputs.in = { node = 0, output = \"out\" }\n", "");
        let new = Project::with_main("show", atd_format::network_from_str(&new_text).unwrap());

        let mut view = DiffView::default();
        view.start(
            Side::Source(atd_format::git::Source::Folder(dir.clone())),
            Side::Open(Box::new(new)),
        );
        let start = std::time::Instant::now();
        while view.busy() {
            view.poll();
            assert!(start.elapsed().as_secs() < 30);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let c = view.compared.as_ref().expect("compared");
        let marks: Vec<(u64, Mark)> = c.marks().into_iter().map(|(id, m)| (id.0, m)).collect();
        assert_eq!(
            marks,
            [(0, Mark::Removed), (1, Mark::Changed), (2, Mark::Added)]
        );
        let d = c.node_diff(NodeId(1)).unwrap();
        let rows: Vec<_> = d.changes.iter().map(row).collect();
        assert_eq!(
            rows[0],
            ("param gamma".into(), "default".into(), "2.0".into())
        );
        assert_eq!(
            rows[1],
            ("input in".into(), "n0.out".into(), "(none)".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
