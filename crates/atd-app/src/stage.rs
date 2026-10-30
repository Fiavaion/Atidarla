//! The Stage tab (M3, FR-S1, FR-S8): the clip grid of the Stage deck, a
//! view and controller over the graph (decision A54). Launches are edits of
//! the layers' `target` parameter made through [`atd_stage::Launcher`];
//! the layers switch, quantise and fade inside their own cooks. The
//! autopilot, one-shot returns and flash releases are app behaviour (not
//! run by `atidarla render`).

use atd_core::graph::{Edit, NodeId, Param};
use atd_gpu::engine::Engine;
use atd_stage::ai::{Feeds, LiveValue};
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
    message: Option<String>,
    /// The Stage model and the engine revision it was read at: read again
    /// only when the network changes, so a frame allocates nothing.
    model: Option<(u64, Stage)>,
    /// AI clip feeds (FR-A5) for the model's revision.
    feeds: Option<(u64, Feeds)>,
    /// Crossfaders this frame, by mixer (keeps its capacity).
    crossfaders: Vec<(NodeId, f64)>,
    /// Batch results to use as clips (FR-B3).
    bin: crate::media_bin::MediaBinPanel,
}

/// The first empty clip slot of the deck's top layer.
fn free_slot(deck: &Deck) -> Option<(NodeId, usize)> {
    let layer = deck.layers.last()?;
    (0..atd_stage::SLOTS)
        .find(|s| layer.clip(*s).is_none())
        .map(|s| (layer.node, s))
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
    /// The Stage model, taken out of the cache (put it back after use);
    /// read again only if the network changed since.
    fn take_model(&mut self, engine: &Engine) -> (u64, Stage) {
        let rev = engine.revision();
        match self.model.take() {
            Some(m) if m.0 == rev => m,
            _ => (rev, Stage::read(engine.network(), engine.registry())),
        }
    }

    /// Advances the Stage one frame (autopilot, one-shot returns) and
    /// returns the deck's mixer, which the frame cooks whatever node is
    /// viewed (`Engine::cook_frame_with`). Allocates nothing while the
    /// network does not change.
    pub fn frame(&mut self, engine: &mut Engine, time: f64) -> Option<NodeId> {
        let model = self.take_model(engine);
        let deck = model.1.deck();
        if self.deck != deck.map(|d| d.mixer) {
            self.launcher = Launcher::default();
            self.held = None;
            self.deck = deck.map(|d| d.mixer);
        }
        if let Some(deck) = deck {
            let edits = self.launcher.tick(deck, now(engine, deck, time));
            self.apply(engine, "Stage autopilot", edits);
        }
        self.feed_ai(engine, &model, time);
        self.model = Some(model);
        self.deck
    }

    /// Feeds the Prompt Clips playing on each deck to the AI nodes that
    /// follow it, as live values (AI clips, FR-A5).
    fn feed_ai(&mut self, engine: &mut Engine, model: &(u64, Stage), time: f64) {
        let mut feeds = match self.feeds.take() {
            Some(f) if f.0 == model.0 => f,
            _ => (
                model.0,
                Feeds::read(engine.network(), engine.registry(), &model.1),
            ),
        };
        if !feeds.1.feeds.is_empty() {
            self.crossfaders.clear();
            for f in &feeds.1.feeds {
                if let Some(x) = engine
                    .live_values()
                    .get(f.mixer, "crossfader")
                    .and_then(|v| v.as_f64())
                {
                    self.crossfaders.push((f.mixer, x));
                }
            }
            let beat = model.1.deck().map_or(time * 2.0, |d| now(engine, d, time));
            let stage = &model.1;
            let crossfaders = &self.crossfaders;
            feeds.1.frame(
                beat,
                |m| crossfaders.iter().find(|c| c.0 == m).map(|c| c.1),
                |layer| {
                    stage
                        .decks
                        .iter()
                        .find_map(|d| d.layer(layer))
                        .and_then(|l| l.target)
                },
                |node, param, v| match v {
                    LiveValue::Value(v) => engine.set_live(node, param, Some(v)),
                    LiveValue::Float(f) => engine.set_live(node, param, Some(&json!(f))),
                    LiveValue::Int(i) => engine.set_live(node, param, Some(&json!(i))),
                },
            );
        }
        self.feeds = Some(feeds);
    }

    /// Whether the Stage needs frames although nothing changes on screen:
    /// a one-shot or flash waiting to return, or the autopilot on.
    pub fn wants_frames(&self) -> bool {
        self.launcher.busy()
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
        let model = self.take_model(engine);
        let free = model.1.deck().and_then(free_slot);
        let bin = &mut self.bin;
        let action = egui::Panel::bottom("stage-media-bin")
            .resizable(true)
            .default_size(170.0)
            .show(ui, |ui| bin.ui(ui, engine, free))
            .inner;
        if let Some((label, edits)) = action {
            self.message = None;
            self.apply(engine, label, edits);
        }
        self.deck_ui(ui, engine, &model.1, time);
        self.model = Some(model);
    }

    fn deck_ui(&mut self, ui: &mut Ui, engine: &mut Engine, stage: &Stage, time: f64) {
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
        self.toolbar(ui, engine, deck, stage, beat);
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
            .min_col_width(96.0)
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
                            // An empty slot takes a Media Bin image.
                            let (rect, r) = ui
                                .allocate_exact_size(egui::vec2(96.0, 40.0), egui::Sense::hover());
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "·",
                                egui::FontId::default(),
                                ui.visuals().weak_text_color(),
                            );
                            if let Some(c) = r.dnd_release_payload::<crate::media_bin::BinClip>() {
                                let id = engine.alloc_id();
                                edits = Some((
                                    "Use media bin clip",
                                    crate::media_bin::clip_edits(
                                        engine.network(),
                                        id,
                                        &c,
                                        Some((layer.node, col)),
                                    ),
                                ));
                            }
                            continue;
                        };
                        let playing =
                            status.map_or(layer.target == Some(col), |s| s.playing == Some(col));
                        let queued = status.is_some_and(|s| s.queued == Some(Some(col)));
                        // State in words as well as colour.
                        let mark = if playing {
                            "> "
                        } else if queued {
                            "... "
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

#[cfg(test)]
mod tests {
    //! CLAUDE.md rule 2 for the Stage's per-frame work.

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use atd_core::graph::{Endpoint, Node, Wire};
    use serde_json::Value;

    use super::*;

    /// Counts allocations on the thread that turned counting on, so other
    /// tests running in parallel do not disturb the count.
    struct Counting;
    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }
    fn tally() {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
    }
    // SAFETY: forwards to the system allocator; only counts calls.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            tally();
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            tally();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
    #[global_allocator]
    static GLOBAL: Counting = Counting;

    fn count(f: impl FnOnce()) -> usize {
        ALLOCS.with(|n| n.set(0));
        COUNTING.with(|c| c.set(true));
        f();
        COUNTING.with(|c| c.set(false));
        ALLOCS.with(Cell::get)
    }

    fn node(e: &mut Engine, ty: &str, name: &str, params: &[(&str, Value)]) -> NodeId {
        let id = e.alloc_id();
        let mut edits = vec![Edit::AddNode {
            node: Node::new(id, ty, name),
        }];
        for (p, v) in params {
            edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
        e.edit("add", edits).unwrap();
        id
    }

    fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
        e.edit(
            "wire",
            [Edit::Connect {
                wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
            }],
        )
        .unwrap();
    }

    /// A deck with a clock, two clips and the autopilot on (waiting long
    /// enough not to launch while measured): after warm-up, frames
    /// allocate nothing, and an edit is still seen.
    #[test]
    fn steady_stage_frames_allocate_nothing() {
        let Some(mut e) = crate::project::tests::engine() else {
            return;
        };
        let clock = node(&mut e, "sig.beat", "clock", &[]);
        let mixer = node(
            &mut e,
            "img.mixer",
            "deck",
            &[
                ("autopilot", json!("next")),
                ("autopilot_beats", json!(1000)),
            ],
        );
        let layer = node(&mut e, "img.layer", "layer", &[("target", json!(0))]);
        for s in 0..2 {
            let c = node(&mut e, "img.constant", &format!("clip{s}"), &[]);
            wire(&mut e, c, layer, &format!("clip{s}"));
        }
        wire(&mut e, clock, layer, "clock");
        wire(&mut e, layer, mixer, "layer0");

        let mut panel = StagePanel::default();
        for f in 0..30 {
            let t = f64::from(f) / 60.0;
            let deck = panel.frame(&mut e, t);
            assert_eq!(deck, Some(mixer));
            e.cook_frame_with(None, &[mixer], t).unwrap();
        }
        assert!(panel.wants_frames(), "the autopilot runs");
        let n = count(|| {
            for f in 30..150 {
                std::hint::black_box(panel.frame(&mut e, f64::from(f) / 60.0));
            }
        });
        assert_eq!(n, 0, "allocations in 120 Stage frames");

        // The cached model follows edits.
        e.edit(
            "target",
            [Edit::SetParam {
                node: layer,
                param: "target".into(),
                value: Some(Param::constant(json!(1))),
            }],
        )
        .unwrap();
        panel.frame(&mut e, 3.0);
        let (rev, stage) = panel.model.as_ref().unwrap();
        assert_eq!(*rev, e.revision());
        assert_eq!(stage.deck().unwrap().layer(layer).unwrap().target, Some(1));
    }
}
