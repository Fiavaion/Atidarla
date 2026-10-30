//! The Timeline tab (M3 FR-T1–T3, `docs/timeline.md`): ruler, playhead,
//! keyframe tracks, markers, cue list, transport and timecode.
//!
//! The panel owns the project's timeline document while it is open and
//! writes it back to the project after every edit (so Save writes
//! `timeline.toml`, with the chased clock and the Stage mode). Its
//! [`Player`] ticks once per frame before the cook and applies tracks as
//! live values (`Engine::set_live`); stopping clears those it still owns.

use atd_core::graph::NodeId;
use atd_core::registry::ParamKind;
use atd_format::timeline::{Cue, CueAction, Interp, Marker, TimecodeClock, TimelineDoc};
use atd_gpu::engine::Engine;
use atd_io::timecode::Timecode;
use atd_timeline::{ChaseState, ClockSource, PlayState, Player};
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use serde_json::Value;

const GUTTER: f32 = 170.0;
const RULER: f32 = 22.0;
const LANE: f32 = 18.0;
const ROW: f32 = 22.0;
const KEY_R: f32 = 5.0;
const PLAYHEAD: Color32 = Color32::from_rgb(240, 90, 70);
const MARKER: Color32 = Color32::from_rgb(240, 190, 70);
const CUE: Color32 = Color32::from_rgb(90, 180, 240);

/// The Timeline tab's state.
#[derive(Debug)]
pub struct TimelinePanel {
    doc: TimelineDoc,
    player: Option<Player>,
    /// Bumped by every edit of `doc`; written to the project when it moves.
    revision: u64,
    written: u64,
    /// The project (`ProjectState::opened`) the document belongs to.
    opened: Option<u64>,
    /// Network shape the player was bound at (node count, ID sum).
    bound: (usize, u64),
    /// Pixels per second, and the time at the left edge.
    zoom: f32,
    scroll: f64,
    /// Selected key: (track, key).
    selected: Option<(usize, usize)>,
    /// The cue whose details show.
    cue: Option<usize>,
    /// "Add key" picker: parameter and component of the selected node.
    pick: Option<(String, Option<usize>)>,
    /// Launch action: slot to play.
    slot: i64,
    message: Option<String>,
}

impl Default for TimelinePanel {
    fn default() -> Self {
        Self {
            doc: TimelineDoc::default(),
            player: None,
            revision: 0,
            written: 0,
            opened: None,
            bound: (0, 0),
            zoom: 40.0,
            scroll: 0.0,
            selected: None,
            cue: None,
            pick: None,
            slot: 0,
            message: None,
        }
    }
}

fn fingerprint(engine: &Engine) -> (usize, u64) {
    let net = engine.network();
    let sum = net.nodes().fold(0u64, |s, n| s.wrapping_add(n.id.0 + 1));
    (net.nodes().count(), sum)
}

impl TimelinePanel {
    /// Once per frame, before the cook: follows project changes, writes
    /// edits to the project's timeline, and ticks the player at local time
    /// `now` (seconds).
    pub fn frame(
        &mut self,
        engine: &mut Engine,
        project: &mut Option<TimelineDoc>,
        opened: u64,
        now: f64,
    ) {
        if self.opened != Some(opened) {
            // Another project: its values go, its timeline comes in.
            if let Some(p) = &mut self.player {
                p.stop(engine);
            }
            *self = Self {
                doc: project.clone().unwrap_or_default(),
                opened: Some(opened),
                zoom: self.zoom,
                ..Self::default()
            };
        }
        if self.revision != self.written {
            *project = Some(self.doc.clone());
            self.written = self.revision;
            if let Some(p) = &mut self.player {
                p.rebind(self.doc.clone(), engine);
            }
        }
        let shape = fingerprint(engine);
        match &mut self.player {
            Some(p) if shape != self.bound => p.rebind(self.doc.clone(), engine),
            Some(_) => {}
            None => self.player = Some(Player::new(self.doc.clone(), engine)),
        }
        self.bound = shape;
        if let Some(p) = &mut self.player {
            p.tick(engine, now);
            if let Some(e) = p.last_error.take() {
                self.message = Some(e);
            }
        }
    }

    /// Nodes the timeline reads this frame (cue conditions, the chased
    /// Timecode In): they cook whatever node is viewed.
    pub fn watched(&self) -> &[NodeId] {
        self.player.as_ref().map_or(&[][..], Player::watched)
    }

    /// Whether the playhead moves (the app keeps frames coming).
    pub fn playing(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| p.transport.state() == PlayState::Playing)
    }

    fn edited(&mut self) {
        self.doc.canonicalize();
        self.revision += 1;
    }

    fn time(&self) -> f64 {
        self.player.as_ref().map_or(0.0, |p| p.transport.time())
    }

    fn seek(&mut self, t: f64) {
        if let Some(p) = &mut self.player {
            p.transport.seek(t, false);
        }
    }

    /// The tab.
    pub fn ui(&mut self, ui: &mut Ui, engine: Option<&mut Engine>, selected: Option<NodeId>) {
        let Some(engine) = engine else {
            ui.weak("No GPU device: the engine is unavailable.");
            return;
        };
        self.transport_ui(ui, engine);
        self.add_ui(ui, engine, selected);
        if let Some(p) = &self.player {
            for e in p.timeline().problems() {
                ui.colored_label(Color32::from_rgb(230, 160, 60), e);
            }
        }
        if let Some(m) = &self.message {
            ui.colored_label(Color32::from_rgb(230, 80, 80), m);
        }
        ui.separator();
        self.tracks_ui(ui, engine);
        egui::ScrollArea::vertical()
            .id_salt("atd-timeline-details")
            .show(ui, |ui| {
                self.key_ui(ui);
                ui.collapsing(format!("Cues ({})", self.doc.cues.len()), |ui| {
                    self.cues_ui(ui, engine, selected);
                });
                ui.collapsing(format!("Markers ({})", self.doc.markers.len()), |ui| {
                    self.markers_ui(ui);
                });
            });
    }

    fn transport_ui(&mut self, ui: &mut Ui, engine: &mut Engine) {
        let fps = self.doc.fps;
        let Some(p) = &mut self.player else { return };
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Stop")
                .on_hover_text("Stop: back to the start, clears the timeline's values")
                .clicked()
            {
                p.stop(engine);
            }
            let playing = p.transport.state() == PlayState::Playing;
            if ui
                .button(if playing { "Pause" } else { "Play" })
                .on_hover_text("Play / pause (Space over the timeline)")
                .clicked()
            {
                p.transport.toggle();
            }
            if ui
                .button("Go")
                .on_hover_text("Fire the next manual cue")
                .clicked()
            {
                p.go();
            }
            let t = p.transport.time();
            let tc = Timecode::from_frame_count((t * fps).round() as i64, fps as f32);
            ui.monospace(format!(
                "{:02}:{:02}:{:02}:{:02}  {t:7.2} s",
                tc.hours, tc.minutes, tc.seconds, tc.frames
            ))
            .on_hover_text(format!("Timecode at {fps} fps, and seconds"));
            ui.separator();
            let mut looping = p.transport.looping;
            if ui.checkbox(&mut looping, "Loop").changed() {
                p.transport.looping = looping;
                if looping && self.doc.loop_region.is_none() {
                    self.doc.loop_region = Some([0.0, self.doc.length.min(8.0)]);
                    self.revision += 1;
                } else if !looping {
                    self.doc.loop_region = None;
                    self.revision += 1;
                }
            }
            if let Some([a, b]) = &mut self.doc.loop_region {
                let ra = ui.add(
                    egui::DragValue::new(a)
                        .speed(0.05)
                        .range(0.0..=*b - 0.01)
                        .suffix(" s"),
                );
                let rb = ui.add(
                    egui::DragValue::new(b)
                        .speed(0.05)
                        .range(*a + 0.01..=self.doc.length)
                        .suffix(" s"),
                );
                if ra.changed() || rb.changed() {
                    self.revision += 1;
                }
            }
            ui.label("Length");
            if ui
                .add(
                    egui::DragValue::new(&mut self.doc.length)
                        .speed(0.1)
                        .range(1.0..=86_400.0)
                        .suffix(" s"),
                )
                .changed()
            {
                self.revision += 1;
            }
            ui.separator();
            // Stage mode and clock are saved in the document; the player
            // takes them when it rebinds (next frame).
            let mut follow = self.doc.follow_stage;
            egui::ComboBox::from_id_salt("atd-timeline-stage")
                .selected_text(if follow {
                    "Follow Stage"
                } else {
                    "Drive Stage"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut follow, false, "Drive Stage")
                        .on_hover_text("Cues launch clips and columns");
                    ui.selectable_value(&mut follow, true, "Follow Stage")
                        .on_hover_text("Launches on the Stage advance the cue list");
                });
            if follow != self.doc.follow_stage {
                self.doc.follow_stage = follow;
                self.revision += 1;
            }
            ui.separator();
            // Clock: the local clock, or a Timecode In node (MTC or LTC).
            let nodes: Vec<(NodeId, String)> = engine
                .network()
                .nodes()
                .filter(|n| n.type_id == "sig.timecode_in")
                .map(|n| (n.id, n.name.clone()))
                .collect();
            let chased = self.doc.clock.map(|c| c.node);
            let current = match chased {
                None => "Internal clock".to_owned(),
                Some(id) => nodes
                    .iter()
                    .find(|(n, _)| *n == id)
                    .map_or("Timecode (no node)".into(), |(_, name)| {
                        format!("Chase {name}")
                    }),
            };
            let mut choice: Option<Option<NodeId>> = None;
            egui::ComboBox::from_id_salt("atd-timeline-clock")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(chased.is_none(), "Internal clock")
                        .clicked()
                    {
                        choice = Some(None);
                    }
                    for (id, name) in &nodes {
                        let on = chased == Some(*id);
                        if ui.selectable_label(on, format!("Chase {name}")).clicked() {
                            choice = Some(Some(*id));
                        }
                    }
                    if nodes.is_empty() {
                        ui.weak("Add a Timecode In node to chase MTC or LTC.");
                    }
                });
            if let Some(c) = choice
                && c != chased
            {
                let offset = self.doc.clock.map_or(0.0, |k| k.offset);
                self.doc.clock = c.map(|node| TimecodeClock { node, offset });
                self.revision += 1;
            }
            if p.transport.clock == ClockSource::Timecode
                && let Some(clock) = &mut self.doc.clock
            {
                let state = match p.transport.chase.state() {
                    ChaseState::NoCode => "no code",
                    ChaseState::Locking => "locking",
                    ChaseState::Locked => "locked",
                    ChaseState::Freewheel => "freewheel",
                };
                ui.label(state);
                if ui
                    .add(
                        egui::DragValue::new(&mut clock.offset)
                            .speed(0.04)
                            .prefix("offset ")
                            .suffix(" s"),
                    )
                    .on_hover_text("Timeline time = timecode + offset")
                    .changed()
                {
                    self.revision += 1;
                }
            }
        });
    }

    /// Adding keys, markers and cues at the playhead.
    fn add_ui(&mut self, ui: &mut Ui, engine: &Engine, selected: Option<NodeId>) {
        let t = self.time();
        ui.horizontal_wrapped(|ui| {
            let node = selected.and_then(|id| engine.network().node(id));
            let schema = node.and_then(|n| engine.registry().get(&n.type_id));
            match (node, schema) {
                (Some(node), Some(schema)) => {
                    let options: Vec<(String, Option<usize>)> = schema
                        .params
                        .iter()
                        .flat_map(|p| match &p.kind {
                            ParamKind::Float { .. } | ParamKind::Int { .. } | ParamKind::Toggle => {
                                vec![(p.name.clone(), None)]
                            }
                            ParamKind::Colour | ParamKind::Vec { .. } => {
                                let n = p.default.as_array().map_or(4, Vec::len);
                                (0..n).map(|c| (p.name.clone(), Some(c))).collect()
                            }
                            _ => Vec::new(),
                        })
                        .collect();
                    if self.pick.as_ref().is_none_or(|p| !options.contains(p)) {
                        self.pick = options.first().cloned();
                    }
                    ui.label(format!("{}:", node.name));
                    let label = |(p, c): &(String, Option<usize>)| match c {
                        Some(c) => format!("{p}[{c}]"),
                        None => p.clone(),
                    };
                    egui::ComboBox::from_id_salt("atd-timeline-param")
                        .selected_text(self.pick.as_ref().map(label).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for o in &options {
                                ui.selectable_value(&mut self.pick, Some(o.clone()), label(o));
                            }
                        });
                    if ui
                        .add_enabled(self.pick.is_some(), egui::Button::new("Key at playhead"))
                        .on_hover_text("Adds a key with the parameter's current value")
                        .clicked()
                        && let Some((param, comp)) = self.pick.clone()
                    {
                        let v = current_value(engine, node.id, &param, comp);
                        let fps = self.doc.fps;
                        let track = self.doc.track_mut(node.id, &param, comp);
                        let k = track.set_key(snap(t, fps), v);
                        self.edited();
                        let ti = self.doc.tracks.iter().position(|tr| {
                            tr.node == node.id && tr.param == param && tr.component == comp
                        });
                        self.selected = ti.map(|ti| (ti, k));
                    }
                }
                _ => {
                    ui.weak("Select a node to key its parameters.");
                }
            }
            ui.separator();
            if ui
                .button("Marker")
                .on_hover_text("Adds a marker at the playhead")
                .clicked()
            {
                let id = self.doc.next_id();
                self.doc.markers.push(Marker {
                    id,
                    t: snap(t, self.doc.fps),
                    name: format!("Marker {id}"),
                });
                self.edited();
            }
            for (label, at) in [("Cue", Some(snap(t, self.doc.fps))), ("Manual cue", None)] {
                if ui
                    .button(label)
                    .on_hover_text("Adds a cue (add its actions in Cues)")
                    .clicked()
                {
                    let id = self.doc.next_id();
                    self.doc.cues.push(Cue {
                        id,
                        name: format!("Cue {id}"),
                        t: at,
                        when: None,
                        actions: Vec::new(),
                    });
                    self.edited();
                    self.cue = self.doc.cues.iter().position(|c| c.id == id);
                }
            }
        });
    }

    /// Ruler, playhead, marker and cue lanes, and one row per track.
    fn tracks_ui(&mut self, ui: &mut Ui, engine: &Engine) {
        let rows = self.doc.tracks.len().max(1) as f32;
        let height = RULER + 2.0 * LANE + rows * ROW + 4.0;
        let width = ui.available_width();
        let (rect, bg) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
        let left = rect.left() + GUTTER;
        let area = Rect::from_min_max(Pos2::new(left, rect.top()), rect.max);

        // Pan with the wheel, zoom with Cmd/Ctrl + wheel.
        if bg.hovered() {
            let (scroll, zoom, cmd) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.command));
            if zoom != 1.0 || (cmd && scroll.y != 0.0) {
                let f = if zoom != 1.0 {
                    zoom
                } else {
                    (scroll.y / 200.0).exp()
                };
                self.zoom = (self.zoom * f).clamp(2.0, 2000.0);
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                self.scroll = (self.scroll - f64::from(dx / self.zoom)).max(0.0);
            }
            if !ui.ctx().egui_wants_keyboard_input()
                && ui.input(|i| i.key_pressed(egui::Key::Space))
                && let Some(p) = &mut self.player
            {
                p.transport.toggle();
            }
        }
        let (scroll, zoom) = (self.scroll, self.zoom);
        let x_of = |t: f64| left + ((t - scroll) as f32) * zoom;
        let t_of = |x: f32| scroll + f64::from((x - left) / zoom);

        // Ruler: a tick every "nice" step about 80 px apart.
        let ruler = Rect::from_min_max(
            Pos2::new(left, rect.top()),
            Pos2::new(rect.right(), rect.top() + RULER),
        );
        painter.rect_filled(ruler, 0.0, visuals.faint_bg_color);
        let step = [
            0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0,
        ]
        .into_iter()
        .find(|s| *s as f32 * zoom >= 80.0)
        .unwrap_or(600.0);
        let mut tick = (scroll / step).floor() * step;
        while x_of(tick) < rect.right() {
            let x = x_of(tick);
            if x >= left {
                painter.vline(
                    x,
                    ruler.y_range(),
                    Stroke::new(1.0, visuals.weak_text_color()),
                );
                painter.vline(
                    x,
                    ruler.bottom()..=rect.bottom(),
                    Stroke::new(1.0, visuals.faint_bg_color),
                );
                painter.text(
                    Pos2::new(x + 3.0, ruler.top() + 2.0),
                    Align2::LEFT_TOP,
                    clock(tick),
                    FontId::monospace(10.0),
                    visuals.weak_text_color(),
                );
            }
            tick += step;
        }
        // The end and the loop region.
        let end = x_of(self.doc.length);
        if end < rect.right() {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(end.max(left), rect.top()), rect.max),
                0.0,
                Color32::from_black_alpha(80),
            );
        }
        if let Some([a, b]) = self.doc.loop_region {
            let r = Rect::from_x_y_ranges(
                x_of(a).max(left)..=x_of(b).max(left),
                ruler.top()..=ruler.top() + 4.0,
            );
            painter.rect_filled(r, 0.0, Color32::from_rgb(120, 200, 120));
        }
        let ruler_r = ui.interact(
            ruler,
            Id::unique("atd-timeline-ruler"),
            Sense::click_and_drag(),
        );
        if (ruler_r.dragged() || ruler_r.clicked())
            && let Some(pos) = ruler_r.interact_pointer_pos()
        {
            self.seek(t_of(pos.x).clamp(0.0, self.doc.length));
        }

        // Markers and cues.
        let lane = |i: f32| {
            Rect::from_min_max(
                Pos2::new(rect.left(), ruler.bottom() + i * LANE),
                Pos2::new(rect.right(), ruler.bottom() + (i + 1.0) * LANE),
            )
        };
        let label = |painter: &egui::Painter, r: Rect, text: &str| {
            painter.text(
                Pos2::new(r.left() + 6.0, r.center().y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(12.0),
                visuals.text_color(),
            );
        };
        label(&painter, lane(0.0), "Markers");
        label(&painter, lane(1.0), "Cues");
        for m in &self.doc.markers {
            let x = x_of(m.t);
            if x < left {
                continue;
            }
            let r = lane(0.0);
            painter.vline(x, r.y_range(), Stroke::new(2.0, MARKER));
            painter.text(
                Pos2::new(x + 3.0, r.center().y),
                Align2::LEFT_CENTER,
                &m.name,
                FontId::proportional(11.0),
                MARKER,
            );
        }
        let mut cue_clicked = None;
        for (i, c) in self.doc.cues.iter().enumerate() {
            let Some(t) = c.t else { continue };
            let x = x_of(t);
            if x < left {
                continue;
            }
            let r = lane(1.0);
            let fired = self.player.as_ref().is_some_and(|p| p.timeline().fired(i));
            let colour = if fired {
                visuals.weak_text_color()
            } else {
                CUE
            };
            let hit = Rect::from_center_size(Pos2::new(x, r.center().y), Vec2::new(10.0, LANE));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x, r.top() + 2.0),
                    Pos2::new(x + 6.0, r.center().y),
                    Pos2::new(x, r.bottom() - 2.0),
                    Pos2::new(x - 6.0, r.center().y),
                ],
                colour,
                Stroke::NONE,
            ));
            painter.text(
                Pos2::new(x + 8.0, r.center().y),
                Align2::LEFT_CENTER,
                &c.name,
                FontId::proportional(11.0),
                colour,
            );
            if ui
                .interact(hit, Id::unique(("atd-timeline-cue", c.id)), Sense::click())
                .on_hover_text(&c.name)
                .clicked()
            {
                cue_clicked = Some(i);
            }
        }
        if cue_clicked.is_some() {
            self.cue = cue_clicked;
        }

        // Tracks.
        let mut select = None;
        let mut moved: Option<(usize, usize, f64)> = None;
        let mut menu: Option<(usize, usize, Option<Interp>)> = None;
        let mut remove_track = None;
        for (ti, track) in self.doc.tracks.iter().enumerate() {
            let top = ruler.bottom() + 2.0 * LANE + ti as f32 * ROW;
            let row = Rect::from_min_max(
                Pos2::new(rect.left(), top),
                Pos2::new(rect.right(), top + ROW),
            );
            if ti % 2 == 1 {
                painter.rect_filled(row, 0.0, visuals.faint_bg_color);
            }
            let name = engine.network().node(track.node).map(|n| n.name.as_str());
            let text = match (name, track.component) {
                (Some(n), Some(c)) => format!("{n}.{}[{c}]", track.param),
                (Some(n), None) => format!("{n}.{}", track.param),
                (None, _) => format!("(node {} gone).{}", track.node.0, track.param),
            };
            let colour = if name.is_some() {
                visuals.text_color()
            } else {
                Color32::from_rgb(230, 80, 80)
            };
            painter.text(
                Pos2::new(rect.left() + 6.0, row.center().y),
                Align2::LEFT_CENTER,
                &text,
                FontId::proportional(12.0),
                colour,
            );
            let name_r = ui.interact(
                Rect::from_min_max(row.min, Pos2::new(left, row.bottom())),
                Id::unique(("atd-timeline-track", ti)),
                Sense::click(),
            );
            name_r.context_menu(|ui| {
                if ui.button("Delete track").clicked() {
                    remove_track = Some(ti);
                    ui.close();
                }
            });
            // The curve, sampled across the visible part.
            let (lo, hi) = track.keys.iter().fold((f64::MAX, f64::MIN), |(lo, hi), k| {
                (lo.min(k.v), hi.max(k.v))
            });
            let span = if hi > lo { hi - lo } else { 1.0 };
            let y_of = |v: f64| row.bottom() - 4.0 - ((v - lo) / span) as f32 * (ROW - 8.0);
            if track.keys.len() > 1 {
                let x0 = x_of(track.keys[0].t).max(left);
                let x1 = x_of(track.keys[track.keys.len() - 1].t).min(rect.right());
                let n = ((x1 - x0) / 3.0).clamp(2.0, 400.0) as usize;
                let pts: Vec<Pos2> = (0..=n)
                    .map(|i| {
                        let x = x0 + (x1 - x0) * i as f32 / n as f32;
                        let v = atd_timeline::curve::sample(&track.keys, t_of(x)).unwrap_or(0.0);
                        Pos2::new(x, y_of(v))
                    })
                    .collect();
                painter.add(egui::Shape::line(
                    pts,
                    Stroke::new(1.0, visuals.weak_text_color()),
                ));
            }
            for (ki, k) in track.keys.iter().enumerate() {
                let x = x_of(k.t);
                if x < left - KEY_R || x > rect.right() + KEY_R {
                    continue;
                }
                let c = Pos2::new(x, row.center().y);
                let hit = Rect::from_center_size(c, Vec2::splat(KEY_R * 2.5));
                let r = ui
                    .interact(
                        hit,
                        Id::unique(("atd-timeline-key", ti, ki)),
                        Sense::click_and_drag(),
                    )
                    .on_hover_text(format!("{:.3} s = {:.4} ({})", k.t, k.v, k.interp.name()));
                let on = self.selected == Some((ti, ki));
                let fill = if on {
                    visuals.selection.bg_fill
                } else {
                    visuals.text_color()
                };
                let shape = match k.interp {
                    Interp::Step => vec![
                        c + Vec2::new(-KEY_R, -KEY_R),
                        c + Vec2::new(KEY_R, -KEY_R),
                        c + Vec2::new(KEY_R, KEY_R),
                        c + Vec2::new(-KEY_R, KEY_R),
                    ],
                    _ => vec![
                        c + Vec2::new(0.0, -KEY_R),
                        c + Vec2::new(KEY_R, 0.0),
                        c + Vec2::new(0.0, KEY_R),
                        c + Vec2::new(-KEY_R, 0.0),
                    ],
                };
                painter.add(egui::Shape::convex_polygon(
                    shape,
                    fill,
                    Stroke::new(1.0, Color32::BLACK),
                ));
                if k.interp == Interp::Bezier {
                    painter.circle_stroke(c, KEY_R + 2.0, Stroke::new(1.0, fill));
                }
                if r.clicked() || r.drag_started() {
                    select = Some((ti, ki));
                }
                if r.dragged()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    moved = Some((
                        ti,
                        ki,
                        snap(t_of(pos.x).clamp(0.0, self.doc.length), self.doc.fps),
                    ));
                }
                r.context_menu(|ui| {
                    for i in Interp::ALL {
                        if ui.selectable_label(k.interp == i, i.name()).clicked() {
                            menu = Some((ti, ki, Some(i)));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Delete key").clicked() {
                        menu = Some((ti, ki, None));
                        ui.close();
                    }
                });
            }
        }
        if self.doc.tracks.is_empty() {
            painter.text(
                Pos2::new(left + 8.0, ruler.bottom() + 2.0 * LANE + ROW / 2.0),
                Align2::LEFT_CENTER,
                "No tracks: select a node, pick a parameter and add a key at the playhead.",
                FontId::proportional(12.0),
                visuals.weak_text_color(),
            );
        }
        if select.is_some() {
            self.selected = select;
        }
        if let Some((ti, ki, t)) = moved {
            let keys = &mut self.doc.tracks[ti].keys;
            if keys[ki].t != t && !keys.iter().any(|k| k.t == t) {
                let mut k = keys.remove(ki);
                k.t = t;
                let at = keys.partition_point(|o| o.t < t);
                keys.insert(at, k);
                self.selected = Some((ti, at));
                self.edited();
            }
        }
        if let Some((ti, ki, what)) = menu {
            match what {
                Some(i) => self.doc.tracks[ti].keys[ki].interp = i,
                None => self.delete_key(ti, ki),
            }
            self.edited();
        }
        if let Some(ti) = remove_track {
            self.doc.tracks.remove(ti);
            self.selected = None;
            self.edited();
        }
        let delete = bg.hovered()
            && !ui.ctx().egui_wants_keyboard_input()
            && ui
                .input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        if delete && let Some((ti, ki)) = self.selected {
            self.delete_key(ti, ki);
            self.edited();
        }
        // Clicking empty space in the track area scrubs there.
        if bg.clicked()
            && let Some(pos) = bg.interact_pointer_pos()
            && area.contains(pos)
        {
            self.selected = None;
            self.seek(t_of(pos.x).clamp(0.0, self.doc.length));
        }
        // Playhead.
        let x = x_of(self.time());
        if x >= left {
            painter.vline(x, rect.y_range(), Stroke::new(1.5, PLAYHEAD));
        }
        if self.playing() {
            ui.ctx().request_repaint();
        }
    }

    fn delete_key(&mut self, ti: usize, ki: usize) {
        let Some(track) = self.doc.tracks.get_mut(ti) else {
            return;
        };
        if ki < track.keys.len() {
            track.keys.remove(ki);
        }
        if track.keys.is_empty() {
            self.doc.tracks.remove(ti);
        }
        self.selected = None;
    }

    /// The selected key's time, value, curve and handles.
    fn key_ui(&mut self, ui: &mut Ui) {
        let Some((ti, ki)) = self.selected else {
            return;
        };
        let Some(k) = self
            .doc
            .tracks
            .get(ti)
            .and_then(|t| t.keys.get(ki))
            .copied()
        else {
            self.selected = None;
            return;
        };
        let mut edit = k;
        ui.horizontal_wrapped(|ui| {
            ui.label("Key");
            ui.add(egui::DragValue::new(&mut edit.t).speed(1.0 / self.doc.fps).range(0.0..=self.doc.length).suffix(" s"));
            ui.add(egui::DragValue::new(&mut edit.v).speed(0.01).prefix("value "));
            egui::ComboBox::from_id_salt("atd-timeline-interp")
                .selected_text(edit.interp.name())
                .show_ui(ui, |ui| {
                    for i in Interp::ALL {
                        ui.selectable_value(&mut edit.interp, i, i.name());
                    }
                });
            if edit.interp == Interp::Bezier || ki > 0 && self.doc.tracks[ti].keys[ki - 1].interp == Interp::Bezier {
                for (label, h) in [("in", &mut edit.tan_in), ("out", &mut edit.tan_out)] {
                    let mut on = h.is_some();
                    ui.checkbox(&mut on, label).on_hover_text("A Bezier handle: [seconds, value] from the key (off: a flat third of the segment)");
                    match (on, h.as_mut()) {
                        (true, Some(v)) => {
                            ui.add(egui::DragValue::new(&mut v[0]).speed(0.01));
                            ui.add(egui::DragValue::new(&mut v[1]).speed(0.01));
                        }
                        (true, None) => *h = Some(if label == "in" { [-0.25, 0.0] } else { [0.25, 0.0] }),
                        (false, _) => *h = None,
                    }
                }
            }
        });
        if edit != k {
            let keys = &mut self.doc.tracks[ti].keys;
            if edit.t != k.t && keys.iter().any(|o| o.t == edit.t) {
                edit.t = k.t;
            }
            keys[ki] = edit;
            keys.sort_by(|a, b| a.t.total_cmp(&b.t));
            self.selected = keys.iter().position(|o| *o == edit).map(|i| (ti, i));
            self.edited();
        }
    }

    fn cues_ui(&mut self, ui: &mut Ui, engine: &Engine, selected: Option<NodeId>) {
        let mut remove = None;
        let mut changed = false;
        let player = self.player.as_ref();
        for (i, c) in self.doc.cues.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let status = player.map_or("", |p| {
                    let tl = p.timeline();
                    if tl.current() == Some(i) {
                        ">"
                    } else if tl.fired(i) {
                        "done"
                    } else if tl.armed(i) {
                        "armed"
                    } else {
                        ""
                    }
                });
                ui.monospace(status)
                    .on_hover_text("> current · done: fired · armed: waiting for its condition");
                if ui
                    .selectable_label(self.cue == Some(i), "Edit")
                    .on_hover_text("Edit actions")
                    .clicked()
                {
                    self.cue = if self.cue == Some(i) { None } else { Some(i) };
                }
                changed |= ui
                    .add(egui::TextEdit::singleline(&mut c.name).desired_width(110.0))
                    .changed();
                let mut timed = c.t.is_some();
                if ui
                    .checkbox(&mut timed, "at")
                    .on_hover_text("Off: a manual cue, fired with Go")
                    .changed()
                {
                    c.t = timed.then_some(0.0);
                    changed = true;
                }
                if let Some(t) = &mut c.t {
                    changed |= ui
                        .add(
                            egui::DragValue::new(t)
                                .speed(0.05)
                                .range(0.0..=f64::MAX)
                                .suffix(" s"),
                        )
                        .changed();
                }
                let mut when = c.when.clone().unwrap_or_default();
                let r = ui
                    .add(
                        egui::TextEdit::singleline(&mut when)
                            .hint_text("when… e.g. op('beat1')['beat'] > 0")
                            .desired_width(200.0),
                    )
                    .on_hover_text("A condition in the native expression subset; empty: none");
                if r.changed() {
                    c.when = (!when.trim().is_empty()).then_some(when);
                    changed = true;
                }
                if let Some(e) = player.and_then(|p| p.timeline().cue_error(i)) {
                    ui.colored_label(Color32::from_rgb(230, 80, 80), e);
                }
                ui.weak(format!("{} action(s)", c.actions.len()));
                if ui
                    .small_button("Delete")
                    .on_hover_text("Delete cue")
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.doc.cues.remove(i);
            self.cue = None;
            changed = true;
        }
        if changed {
            self.edited();
        }
        let Some(ci) = self.cue.filter(|c| *c < self.doc.cues.len()) else {
            if self.doc.cues.is_empty() {
                ui.weak("No cues: add one at the playhead, or a manual cue for Go.");
            }
            return;
        };
        ui.separator();
        let name = |id: NodeId| {
            engine
                .network()
                .node(id)
                .map_or(format!("node {}", id.0), |n| n.name.clone())
        };
        let mut remove_action = None;
        ui.label(format!("Actions of {}", self.doc.cues[ci].name));
        for (ai, a) in self.doc.cues[ci].actions.iter().enumerate() {
            ui.horizontal(|ui| {
                let text = match a {
                    CueAction::Set { node, param, value } => {
                        format!("set {}.{param} = {value}", name(*node))
                    }
                    CueAction::Live { node, param, value } => {
                        format!("live {}.{param} = {value}", name(*node))
                    }
                    CueAction::Launch { layers, slot } => format!(
                        "launch slot {slot} on {}",
                        layers
                            .iter()
                            .map(|l| name(*l))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                ui.monospace(text);
                if ui.small_button("Delete").clicked() {
                    remove_action = Some(ai);
                }
            });
        }
        if let Some(ai) = remove_action {
            self.doc.cues[ci].actions.remove(ai);
            self.edited();
        }
        ui.horizontal_wrapped(|ui| {
            let pick = self.pick.clone().filter(|(_, c)| c.is_none());
            let target = selected.zip(pick);
            let enabled = target.is_some();
            let value = target.clone().and_then(|(n, (p, _))| engine.network().param_value(engine.registry(), n, &p).cloned());
            for (label, live) in [("+ set", false), ("+ live", true)] {
                if ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .on_hover_text("The selected node's picked parameter, at its current value (edit the value in timeline.toml)")
                    .clicked()
                    && let (Some((node, (param, _))), Some(value)) = (target.clone(), value.clone())
                {
                    self.doc.cues[ci].actions.push(if live {
                        CueAction::Live { node, param, value }
                    } else {
                        CueAction::Set { node, param, value }
                    });
                    self.edited();
                }
            }
            ui.separator();
            ui.add(egui::DragValue::new(&mut self.slot).range(-1..=15).prefix("slot "));
            let layer = selected.filter(|id| {
                engine
                    .network()
                    .node(*id)
                    .and_then(|n| engine.registry().get(&n.type_id))
                    .is_some_and(|s| s.params.iter().any(|p| p.name == "target"))
            });
            if ui
                .add_enabled(layer.is_some(), egui::Button::new("+ launch"))
                .on_hover_text("Play this slot on the selected layer (a node with `target`); add more layers to launch a column")
                .clicked()
                && let Some(layer) = layer
            {
                let actions = &mut self.doc.cues[ci].actions;
                let slot = self.slot;
                match actions.iter_mut().find_map(|a| match a {
                    CueAction::Launch { layers, slot: s } if *s == slot => Some(layers),
                    _ => None,
                }) {
                    Some(layers) if !layers.contains(&layer) => layers.push(layer),
                    Some(_) => {}
                    None => actions.push(CueAction::Launch { layers: vec![layer], slot }),
                }
                self.edited();
            }
        });
    }

    fn markers_ui(&mut self, ui: &mut Ui) {
        let mut remove = None;
        let mut changed = false;
        let mut jump = None;
        for (i, m) in self.doc.markers.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                if ui
                    .small_button("Go to")
                    .on_hover_text("Move the playhead here")
                    .clicked()
                {
                    jump = Some(m.t);
                }
                changed |= ui
                    .add(egui::TextEdit::singleline(&mut m.name).desired_width(140.0))
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut m.t)
                            .speed(0.05)
                            .range(0.0..=f64::MAX)
                            .suffix(" s"),
                    )
                    .changed();
                if ui
                    .small_button("Delete")
                    .on_hover_text("Delete marker")
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.doc.markers.remove(i);
            changed = true;
        }
        if changed {
            self.edited();
        }
        if let Some(t) = jump {
            self.seek(t);
        }
    }
}

/// The value a new key takes: what the parameter shows now (a live value
/// if one is set, else the stored one).
fn current_value(engine: &Engine, node: NodeId, param: &str, comp: Option<usize>) -> f64 {
    let v = engine
        .live_values()
        .get(node, param)
        .or_else(|| engine.network().param_value(engine.registry(), node, param));
    let v = match (v, comp) {
        (Some(v), Some(c)) => v.get(c),
        (v, None) => v,
        _ => None,
    };
    v.and_then(|v| match v {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        v => v.as_f64(),
    })
    .unwrap_or(0.0)
}

/// `t` on the nearest frame.
fn snap(t: f64, fps: f64) -> f64 {
    (t * fps).round() / fps
}

/// `m:ss` or `m:ss.s` for ruler labels.
fn clock(t: f64) -> String {
    let m = (t / 60.0).floor();
    let s = t - m * 60.0;
    if (s - s.round()).abs() < 1e-6 {
        format!("{m}:{:02}", s.round() as i64)
    } else {
        format!("{m}:{s:04.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_labels_and_snapping() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(75.0), "1:15");
        assert_eq!(clock(2.5), "0:02.5");
        assert_eq!(snap(1.0 / 60.0 + 0.003, 60.0), 1.0 / 60.0);
    }
}
