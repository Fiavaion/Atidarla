//! The Stage tab (M3, FR-S1, FR-S8): the clip grid of the Stage deck, a
//! view and controller over the graph (decision A54). Launches are edits of
//! the layers' `target` parameter made through [`atd_stage::Launcher`];
//! the layers switch, quantise and fade inside their own cooks.

use atd_core::graph::{Edit, NodeId, Param};
use atd_gpu::engine::{Engine, EngineError};
use atd_stage::{Deck, LaunchMode, Launcher, LayerStatus, Stage};
use egui::{Color32, RichText, Ui};
use serde_json::json;

const PLAYING: Color32 = Color32::from_rgb(40, 120, 60);
const QUEUED: Color32 = Color32::from_rgb(150, 110, 20);

#[derive(Debug, Default)]
pub struct StagePanel {
    launcher: Launcher,
    /// The deck the launcher's state belongs to.
    deck: Option<NodeId>,
    /// A flash clip held down: (layer, slot).
    held: Option<(NodeId, usize)>,
    /// Nodes requested this frame, reused.
    requested: Vec<NodeId>,
    message: Option<String>,
}

/// The deck's beat position: its clock's `beat` + `beat_phase`, or
/// seconds at 120 BPM without one.
fn now(engine: &Engine, deck: &Deck, time: f64) -> f64 {
    let ch = deck.clock().and_then(|c| engine.channels(c));
    atd_stage::beat_position(
        ch.and_then(|c| c.value("beat")),
        ch.and_then(|c| c.value("beat_phase")),
        time,
    )
}

impl StagePanel {
    /// Cooks one frame like [`Engine::cook_frame`] (the viewed node and
    /// every sink), plus the Stage deck, so the show keeps playing whatever
    /// is viewed; first runs one-shot returns and the autopilot.
    pub fn cook_frame(
        &mut self,
        engine: &mut Engine,
        viewed: Option<NodeId>,
        time: f64,
    ) -> Result<usize, EngineError> {
        let stage = Stage::read(engine.network(), engine.registry());
        let deck = stage.deck();
        if self.deck != deck.map(|d| d.mixer) {
            self.launcher = Launcher::default();
            self.held = None;
            self.deck = deck.map(|d| d.mixer);
        }
        if let Some(deck) = deck {
            let edits = self.launcher.tick(deck, now(engine, deck, time));
            self.apply(engine, "Stage autopilot", edits);
        }
        let mut requested = std::mem::take(&mut self.requested);
        requested.clear();
        requested.extend(viewed);
        requested.extend(deck.map(|d| d.mixer).filter(|m| Some(*m) != viewed));
        let registry = engine.registry();
        for node in engine.network().nodes() {
            if registry.get(&node.type_id).is_some_and(|s| s.sink) && Some(node.id) != viewed {
                requested.push(node.id);
            }
        }
        let result = engine.cook(&requested, time);
        self.requested = requested;
        result
    }

    fn apply(&mut self, engine: &mut Engine, label: &str, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        if let Err(e) = engine.edit(label, edits) {
            self.message = Some(e.to_string());
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, engine: &mut Engine, time: f64) {
        let stage = Stage::read(engine.network(), engine.registry());
        let Some(deck) = stage.deck() else {
            ui.centered_and_justified(|ui| {
                ui.weak(
                    "No deck yet. Add a Mixer (img.mixer), wire Layers (img.layer) into it, and wire any \
                     image nodes into a layer's clip slots: they appear here as a grid.",
                )
            });
            return;
        };
        let beat = now(engine, deck, time);
        self.toolbar(ui, engine, deck, &stage, beat);
        if let Some(msg) = &self.message {
            ui.colored_label(ui.visuals().error_fg_color, msg);
        }
        ui.separator();
        egui::ScrollArea::both().show(ui, |ui| self.grid(ui, engine, deck, beat));
        // A held flash returns when the button is let go anywhere.
        if let Some((layer, slot)) = self.held
            && !ui.input(|i| i.pointer.primary_down())
        {
            self.held = None;
            if let Some(l) = deck.layer(layer) {
                let edits = self.launcher.release(l, slot);
                self.apply(engine, "Stage flash release", edits);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, engine: &mut Engine, deck: &Deck, stage: &Stage, beat: f64) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(&deck.name);
            if stage.decks.len() > 1 {
                ui.weak(format!(
                    "(1 of {} decks: set Stage Deck on a Mixer to show another)",
                    stage.decks.len()
                ));
            }
            ui.separator();
            if ui
                .button("Stop all")
                .on_hover_text("Stops every layer of the deck (one undo step)")
                .clicked()
            {
                let edits = self.launcher.stop_all(deck);
                self.apply(engine, "Stage stop all", edits);
            }
            ui.separator();
            for (param, label, value) in [
                ("crossfader", "Crossfader A/B", deck.crossfader),
                ("master", "Master", deck.master),
            ] {
                let mut v = value;
                let r = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).text(label));
                if r.changed() {
                    let edit = Edit::SetParam {
                        node: deck.mixer,
                        param: param.into(),
                        value: Some(Param::constant(json!(v))),
                    };
                    let key = format!("{}:{param}", deck.mixer);
                    if let Err(e) =
                        engine.edit_coalesced(&format!("{}.{param}", deck.name), &key, [edit])
                    {
                        self.message = Some(e.to_string());
                    }
                }
                if r.drag_stopped() {
                    engine.seal_history();
                }
            }
            ui.separator();
            let autopilot = match deck.autopilot.mode {
                atd_stage::AutopilotMode::Off => "off".to_owned(),
                atd_stage::AutopilotMode::Next => "next column".to_owned(),
                atd_stage::AutopilotMode::Random => "random column".to_owned(),
                atd_stage::AutopilotMode::Column(c) => format!("column {}", c + 1),
            };
            ui.label(format!("Autopilot: {autopilot}"))
                .on_hover_text("Set on the Mixer's Autopilot parameters");
            ui.weak(format!("beat {:.1}", beat));
        });
    }

    fn grid(&mut self, ui: &mut Ui, engine: &mut Engine, deck: &Deck, beat: f64) {
        let columns = deck.columns().max(1);
        let mut edits: Option<(&str, Vec<Edit>)> = None;
        egui::Grid::new(("stage-grid", deck.mixer))
            .striped(true)
            .spacing([6.0, 6.0])
            .show(ui, |ui| {
                ui.weak("Layer");
                for col in 0..columns {
                    if ui
                        .button(format!("Column {}", col + 1))
                        .on_hover_text("Launches this column on every layer (one undo step)")
                        .clicked()
                    {
                        edits = Some(("Stage column", self.launcher.column(deck, col, beat)));
                    }
                }
                ui.end_row();
                // Top layer first, as it sits on top of the composite.
                for layer in deck.layers.iter().rev() {
                    let status = engine.status(layer.node).and_then(LayerStatus::parse);
                    ui.vertical(|ui| {
                        ui.strong(&layer.name);
                        if ui
                            .small_button("Stop")
                            .on_hover_text("Stops this layer")
                            .clicked()
                        {
                            edits = Some(("Stage stop", self.launcher.stop(layer)));
                        }
                        for w in &layer.warnings {
                            ui.colored_label(ui.visuals().warn_fg_color, w);
                        }
                    });
                    for col in 0..columns {
                        let Some(clip) = layer.clip(col) else {
                            ui.weak("·");
                            continue;
                        };
                        let playing =
                            status.map_or(layer.target == Some(col), |s| s.playing == Some(col));
                        let queued = status.is_some_and(|s| s.queued == Some(Some(col)));
                        // State in words as well as colour.
                        let mark = if playing {
                            "▶ "
                        } else if queued {
                            "… "
                        } else {
                            ""
                        };
                        let mut button =
                            egui::Button::new(RichText::new(format!("{mark}{}", clip.label)))
                                .min_size(egui::vec2(96.0, 40.0))
                                .sense(egui::Sense::click_and_drag());
                        if playing {
                            button = button.fill(PLAYING);
                        } else if queued {
                            button = button.fill(QUEUED);
                        }
                        let mode = match clip.mode {
                            LaunchMode::Trigger => "trigger",
                            LaunchMode::Toggle => "toggle: again stops",
                            LaunchMode::Flash => "flash: plays while held",
                            LaunchMode::OneShot => "one-shot: returns after its length",
                        };
                        let r = ui.add(button).on_hover_text(format!(
                            "{} · slot {col} · {mode}{}",
                            clip.label,
                            if layer.target_driven {
                                " · target follows a channel; launching replaces it"
                            } else {
                                ""
                            }
                        ));
                        if clip.mode == LaunchMode::Flash {
                            if r.is_pointer_button_down_on() && self.held.is_none() {
                                self.held = Some((layer.node, col));
                                edits =
                                    Some(("Stage flash", self.launcher.press(layer, col, beat)));
                            }
                        } else if r.clicked() {
                            edits = Some(("Stage launch", self.launcher.press(layer, col, beat)));
                        }
                    }
                    ui.end_row();
                }
            });
        if let Some((label, e)) = edits {
            self.message = None;
            self.apply(engine, label, e);
        }
    }
}
