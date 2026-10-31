//! MIDI/OSC mappings in the app (FR-S5, docs/mappings.md): the runtime
//! applied each frame, learn from the Parameters panel, the Mappings
//! window (list, edit, delete, live input monitor) and OSCQuery.
//!
//! The project's mappings live in `ProjectState::meta.mappings` (saved as
//! `mappings.toml`); this module holds what runs: the I/O, learn state and
//! the window. Mapped values are live values (A54): not edits, not saved.
//! Saving stores what each mapping last wrote as its parameter's value
//! ([`Control::stored_values`]), so a show reopens where it was left.

use atd_core::graph::{Edit, NodeId, Param};
use atd_core::registry::ParamSchema;
use atd_format::Mappings;
use atd_format::mappings::{Curve, Mode, Source, Target};
use atd_gpu::engine::Engine;
use atd_io::mapping::{self, MappingRuntime, Waker};
use atd_io::oscquery::{self, OscQueryServer, ServerOptions};
use egui::{RichText, Ui};

/// What runs for the open project's mappings.
pub struct Control {
    rt: MappingRuntime,
    server: Option<OscQueryServer>,
    /// Network structure the routes were built for.
    fingerprint: Option<u64>,
    /// `ProjectState::opened` last seen.
    opened: Option<u64>,
    /// The OSC port wanted and the result of binding it.
    osc: Option<(u16, Result<u16, String>)>,
    server_error: Option<String>,
    midi_open: bool,
    /// `--oscquery`: advertise even if the project does not ask to.
    force_oscquery: bool,
    /// The Mappings window is shown.
    pub open: bool,
    waker_set: bool,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control").finish_non_exhaustive()
    }
}

impl Control {
    pub fn new(force_oscquery: bool) -> Self {
        Self {
            rt: MappingRuntime::new(),
            server: None,
            fingerprint: None,
            opened: None,
            osc: None,
            server_error: None,
            midi_open: false,
            force_oscquery,
            open: false,
            waker_set: false,
        }
    }

    /// Once per frame, before the cook: I/O to match the project, learn,
    /// OSCQuery, and the latest mapped values as live values.
    pub fn frame(
        &mut self,
        ctx: &egui::Context,
        engine: &mut Engine,
        maps: &mut Mappings,
        name: &str,
        opened: u64,
    ) {
        if !self.waker_set {
            let c = ctx.clone();
            self.rt
                .hub()
                .set_waker(Some(Waker(std::sync::Arc::new(move || {
                    c.request_repaint()
                }))));
            self.waker_set = true;
        }
        if self.opened != Some(opened) {
            // Another project: the old one's live values must not land on
            // nodes that happen to share an ID.
            self.opened = Some(opened);
            engine.clear_live();
            self.rt.forget_written();
            self.rt.cancel_learn();
            self.fingerprint = None;
        }
        self.io(maps);
        self.oscquery(ctx, engine, maps, name);
        if self
            .rt
            .learn_into(maps, |t| {
                param_schema(engine, t.node, &t.param).map(|p| p.kind)
            })
            .is_some()
        {
            self.open = true;
        }
        self.apply(engine, maps);
    }

    /// The latest mapped values as live values. A parameter no mapping
    /// drives any more (deleted or retargeted) gets its live value cleared,
    /// unless another writer has set it since.
    fn apply(&mut self, engine: &mut Engine, maps: &Mappings) {
        self.rt.update(
            maps,
            |n, p| {
                mapping::resolve(
                    engine.network(),
                    engine.registry(),
                    n,
                    p,
                    engine.live_values().get(n, p),
                )
            },
            |n, p, i| engine.driven_component(n, p, i),
        );
        for (n, p, v) in self.rt.released() {
            engine.clear_live_if(n, p, v);
        }
        for (n, p, v) in self.rt.writes() {
            engine.set_live(n, p, Some(v));
        }
    }

    /// Edits storing what each mapping last wrote as its parameter's value
    /// (only the components mappings drive), for saving: a show reopens
    /// where the controls left it. Bindings and expressions stay;
    /// parameters no mapping has written, and live values from the
    /// timeline or OSCQuery, are left alone.
    pub fn stored_values(&self, engine: &Engine) -> Vec<Edit> {
        store_edits(engine, self.rt.written())
    }

    /// Opens or closes the OSC socket and MIDI inputs the project needs.
    fn io(&mut self, maps: &Mappings) {
        let wants_osc = self.oscquery_on(maps)
            || self.open
            || self.rt.armed().is_some()
            || maps
                .mappings
                .iter()
                .any(|m| matches!(m.source, Source::Osc { .. }));
        let port = maps.osc.port;
        match (&self.osc, wants_osc) {
            (Some((p, _)), true) if *p == port => {}
            (_, true) => {
                let r = self
                    .rt
                    .listen_osc(port, false)
                    .map_err(|e| format!("OSC port {port}: {e}"));
                if let Err(e) = &r {
                    log::warn!("{e}");
                }
                self.osc = Some((port, r));
            }
            (Some(_), false) => {
                self.rt.stop_osc();
                self.osc = None;
            }
            (None, false) => {}
        }
        // MIDI inputs open once something needs them and stay open; on
        // Windows they are exclusive, so not before (docs/mappings.md).
        let wants_midi = self.open
            || self.rt.armed().is_some()
            || maps
                .mappings
                .iter()
                .any(|m| matches!(m.source, Source::Midi { .. }));
        if wants_midi && !self.midi_open {
            self.rt.open_midi();
            self.midi_open = true;
        }
    }

    fn oscquery_on(&self, maps: &Mappings) -> bool {
        maps.osc.oscquery || self.force_oscquery
    }

    fn oscquery(&mut self, ctx: &egui::Context, engine: &Engine, maps: &Mappings, name: &str) {
        let wanted = self.oscquery_on(maps);
        let port = maps.osc.oscquery_port;
        if self.server.as_ref().is_some_and(|s| s.port() != port) && port != 0 {
            self.server = None;
        }
        if !wanted {
            if self.server.take().is_some() {
                self.rt.set_routes(Vec::new());
                self.fingerprint = None;
            }
            self.server_error = None;
            return;
        }
        if self.server.is_none() && self.server_error.is_none() {
            match OscQueryServer::start(ServerOptions {
                name: if name.is_empty() { "Atidarla" } else { name }.to_owned(),
                http_port: port,
                osc_port: maps.osc.port,
                loopback: false,
                mdns: true,
            }) {
                Ok(s) => {
                    let c = ctx.clone();
                    s.set_waker(Some(Waker(std::sync::Arc::new(move || {
                        c.request_repaint()
                    }))));
                    self.server = Some(s);
                    self.fingerprint = None;
                }
                Err(e) => self.server_error = Some(format!("OSCQuery port {port}: {e}")),
            }
        }
        let Some(server) = &self.server else { return };
        let fp = oscquery::fingerprint(engine.network());
        let changed = self.fingerprint != Some(fp);
        if changed || server.wants_values() {
            let list = oscquery::advertise(engine.network(), engine.registry(), |n, p| {
                engine.live_values().get(n, p)
            });
            if changed {
                self.rt.set_routes(oscquery::routes(&list));
                self.fingerprint = Some(fp);
            }
            server.publish(list);
        }
    }

    /// The parameter waiting for a control, if any.
    fn armed(&self) -> Option<Target> {
        self.rt.armed()
    }

    /// Adds learn to a parameter's label (right-click) and marks mapped
    /// and armed parameters. Called by the Parameters panel.
    pub fn param_label(&mut self, label: &egui::Response, node: NodeId, p: &ParamSchema) {
        if !mapping::mappable(&p.kind) {
            return;
        }
        let armed = self.armed().filter(|t| t.node == node && t.param == p.name);
        let mapped: Vec<String> = self
            .rt
            .targets()
            .filter(|t| t.node == node && t.param == p.name)
            .map(|t| match t.component {
                Some(c) => format!("component {c}"),
                None => "mapped".to_owned(),
            })
            .collect();
        let ui_painter = label.ctx.layer_painter(label.layer_id);
        if armed.is_some() || !mapped.is_empty() {
            let colour = if armed.is_some() {
                egui::Color32::from_rgb(240, 170, 40)
            } else {
                egui::Color32::from_rgb(90, 170, 240)
            };
            ui_painter.circle_filled(label.rect.left_center() - egui::vec2(5.0, 0.0), 2.5, colour);
        }
        let n = mapping::components(&p.kind);
        let label = if armed.is_some() {
            label
                .clone()
                .on_hover_text("Waiting for a MIDI or OSC control… (right-click to cancel)")
        } else if !mapped.is_empty() {
            label
                .clone()
                .on_hover_text(format!("MIDI/OSC: {}", mapped.join(", ")))
        } else {
            label.clone()
        };
        label.context_menu(|ui| {
            if armed.is_some() {
                if ui.button("Cancel learn").clicked() {
                    self.rt.cancel_learn();
                    ui.close();
                }
                return;
            }
            let arm = |ui: &mut Ui, text: String, component: Option<usize>| {
                if ui
                    .button(text)
                    .on_hover_text("Move a MIDI control or send OSC to map it here")
                    .clicked()
                {
                    self.rt.arm(Target {
                        node,
                        param: p.name.clone(),
                        component,
                    });
                    ui.close();
                }
            };
            if n == 0 {
                arm(ui, "Learn MIDI/OSC".to_owned(), None);
            } else {
                const NAMES: [&str; 4] = ["x / red", "y / green", "z / blue", "w / alpha"];
                for (c, name) in NAMES.iter().enumerate().take(n) {
                    arm(ui, format!("Learn {name}"), Some(c));
                }
                arm(ui, "Learn all components".to_owned(), None);
            }
            if ui.button("Mappings…").clicked() {
                self.open = true;
                ui.close();
            }
        });
    }

    /// The Mappings window: settings, the list (edit, delete) and the
    /// live input monitor.
    pub fn window(&mut self, ctx: &egui::Context, engine: Option<&Engine>, maps: &mut Mappings) {
        let mut open = self.open;
        egui::Window::new("Mappings")
            .open(&mut open)
            .default_size([620.0, 480.0])
            .show(ctx, |ui| self.window_ui(ui, engine, maps));
        self.open = open;
        if open {
            // The monitor updates as messages arrive.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn window_ui(&mut self, ui: &mut Ui, engine: Option<&Engine>, maps: &mut Mappings) {
        self.status_ui(ui, maps);
        ui.separator();
        if let Some(t) = self.armed() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(240, 170, 40),
                    format!("Learning {}: move a control…", target_label(engine, &t)),
                );
                if ui.button("Cancel").clicked() {
                    self.rt.cancel_learn();
                }
            });
        }
        ui.strong(format!("{} mappings", maps.mappings.len()));
        let mut remove = None;
        egui::ScrollArea::vertical()
            .id_salt("atd-mappings-list")
            .max_height(260.0)
            .show(ui, |ui| {
                for m in &mut maps.mappings {
                    ui.push_id(m.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(m.source.label());
                            ui.weak("→");
                            let target = target_label(engine, &m.target);
                            let missing =
                                engine.is_some_and(|e| e.network().node(m.target.node).is_none());
                            if missing {
                                ui.colored_label(egui::Color32::from_rgb(230, 80, 80), target)
                                    .on_hover_text("This node is not in the patch");
                            } else {
                                ui.label(target);
                            }
                            if ui.small_button("delete").clicked() {
                                remove = Some(m.id);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.weak("in");
                            ui.add(egui::DragValue::new(&mut m.input[0]).speed(0.1));
                            ui.add(egui::DragValue::new(&mut m.input[1]).speed(0.1));
                            ui.weak("out");
                            ui.add(egui::DragValue::new(&mut m.output[0]).speed(0.01));
                            ui.add(egui::DragValue::new(&mut m.output[1]).speed(0.01));
                            curve_ui(ui, m.id, &mut m.curve);
                            egui::ComboBox::from_id_salt(("atd-map-mode", m.id))
                                .selected_text(m.mode.name())
                                .show_ui(ui, |ui| {
                                    for mode in Mode::ALL {
                                        ui.selectable_value(&mut m.mode, mode, mode.name());
                                    }
                                });
                            ui.checkbox(&mut m.pickup, "pick-up").on_hover_text(
                                "Soft takeover: wait until the control reaches the value",
                            );
                        });
                    });
                    ui.separator();
                }
            });
        if let Some(id) = remove {
            maps.remove(id);
        }
        ui.separator();
        self.monitor_ui(ui);
    }

    fn status_ui(&mut self, ui: &mut Ui, maps: &mut Mappings) {
        ui.horizontal(|ui| {
            ui.label("OSC port");
            ui.add(egui::DragValue::new(&mut maps.osc.port).range(1..=65535));
            match &self.osc {
                Some((_, Ok(p))) => ui.weak(format!("listening on {p}")),
                Some((_, Err(e))) => ui.colored_label(egui::Color32::from_rgb(230, 80, 80), e),
                None => ui.weak("not listening"),
            };
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut maps.osc.oscquery, "OSCQuery")
                .on_hover_text("Advertise every parameter over HTTP JSON and mDNS (_oscjson._tcp)");
            ui.label("HTTP port");
            ui.add(egui::DragValue::new(&mut maps.osc.oscquery_port).range(1..=65535));
            if self.force_oscquery {
                ui.weak("(on: --oscquery)");
            }
            if let Some(e) = &self.server_error {
                ui.colored_label(egui::Color32::from_rgb(230, 80, 80), e);
                if ui.small_button("retry").clicked() {
                    self.server_error = None;
                }
            } else if let Some(s) = &self.server {
                ui.weak(format!("http://localhost:{}/", s.port()));
                if let Some(e) = s.mdns_error() {
                    ui.colored_label(egui::Color32::from_rgb(230, 80, 80), format!("mDNS: {e}"));
                }
            }
        });
        let inputs = self.rt.midi_inputs();
        ui.horizontal_wrapped(|ui| {
            ui.label("MIDI in:");
            if inputs.is_empty() {
                ui.weak("none");
            }
            for (name, error) in &inputs {
                match error {
                    Some(e) => ui
                        .colored_label(egui::Color32::from_rgb(230, 80, 80), name)
                        .on_hover_text(e),
                    None => ui.label(name),
                };
            }
            if ui
                .small_button("rescan")
                .on_hover_text("Open MIDI inputs plugged in since")
                .clicked()
            {
                self.rt.open_midi();
                self.midi_open = true;
            }
        });
    }

    fn monitor_ui(&self, ui: &mut Ui) {
        let log = self.rt.monitor();
        ui.horizontal(|ui| {
            ui.strong("Input monitor");
            ui.weak(format!("{} messages", self.rt.hub().received()));
        });
        egui::ScrollArea::vertical()
            .id_salt("atd-mappings-monitor")
            .stick_to_bottom(true)
            .max_height(160.0)
            .show(ui, |ui| {
                if log.is_empty() {
                    ui.weak("Move a MIDI control or send OSC to see it here.");
                }
                let now = std::time::Instant::now();
                for e in &log {
                    let age = now.saturating_duration_since(e.at).as_secs_f32();
                    ui.label(RichText::new(format!("{age:>6.1}s  {}", e.text)).monospace());
                }
            });
    }
}

fn curve_ui(ui: &mut Ui, id: u64, curve: &mut Curve) {
    let name = match curve {
        Curve::Linear => "linear",
        Curve::Exp(_) => "exp",
        Curve::Step(_) => "step",
    };
    egui::ComboBox::from_id_salt(("atd-map-curve", id))
        .selected_text(name)
        .show_ui(ui, |ui| {
            if ui.selectable_label(name == "linear", "linear").clicked() {
                *curve = Curve::Linear;
            }
            if ui.selectable_label(name == "exp", "exponential").clicked() && name != "exp" {
                *curve = Curve::Exp(2.0);
            }
            if ui.selectable_label(name == "step", "step").clicked() && name != "step" {
                *curve = Curve::Step(4);
            }
        });
    match curve {
        Curve::Exp(a) => {
            ui.add(egui::DragValue::new(a).speed(0.05).range(-20.0..=20.0))
                .on_hover_text("Amount: above 0 starts slow, below 0 starts fast");
        }
        Curve::Step(n) => {
            ui.add(egui::DragValue::new(n).range(2..=128))
                .on_hover_text("Levels");
        }
        Curve::Linear => {}
    }
}

fn param_schema(engine: &Engine, node: NodeId, param: &str) -> Option<ParamSchema> {
    let n = engine.network().node(node)?;
    engine
        .registry()
        .get(&n.type_id)?
        .params
        .iter()
        .find(|p| p.name == param)
        .cloned()
}

fn target_label(engine: Option<&Engine>, t: &Target) -> String {
    let node = engine
        .and_then(|e| e.network().node(t.node))
        .map_or_else(|| t.node.to_string(), |n| n.name.clone());
    match t.component {
        Some(c) => format!("{node}.{}[{c}]", t.param),
        None => format!("{node}.{}", t.param),
    }
}

/// Edits setting each target's stored value to what its mapping wrote,
/// component by component; unchanged values give no edit.
fn store_edits<'a>(engine: &Engine, written: impl Iterator<Item = (&'a Target, f64)>) -> Vec<Edit> {
    let (net, reg) = (engine.network(), engine.registry());
    let mut values: Vec<(NodeId, &str, serde_json::Value)> = Vec::new();
    for (t, y) in written {
        let Some((kind, stored)) = mapping::resolve(net, reg, t.node, &t.param, None) else {
            continue;
        };
        let at = values
            .iter()
            .position(|(n, p, _)| *n == t.node && *p == t.param);
        let current = at.map_or(stored, |i| &values[i].2);
        let Some(v) = mapping::live_value(kind, current, t.component, y) else {
            continue;
        };
        match at {
            Some(i) => values[i].2 = v,
            None => values.push((t.node, &t.param, v)),
        }
    }
    values
        .into_iter()
        .filter_map(|(node, param, value)| {
            if net.param_value(reg, node, param) == Some(&value) {
                return None;
            }
            let mut p = net
                .node(node)?
                .params
                .get(param)
                .cloned()
                .unwrap_or_else(|| Param::constant(value.clone()));
            p.value = value;
            Some(Edit::SetParam {
                node,
                param: param.to_owned(),
                value: Some(p),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::registry::ParamKind;
    use atd_format::mappings::MidiControl;

    /// What learn gives a parameter kind.
    fn learned_mode(kind: &ParamKind) -> Mode {
        let mut maps = Mappings::default();
        let rt = MappingRuntime::new();
        rt.arm(Target {
            node: NodeId(0),
            param: "p".into(),
            component: None,
        });
        rt.hub().push_midi(
            "d",
            atd_io::midi::Message::Control {
                channel: 1,
                controller: 1,
                value: 1,
            },
        );
        rt.learn_into(&mut maps, |_| Some(kind.clone()));
        maps.mappings[0].mode
    }

    #[test]
    fn learn_picks_a_mode_for_the_parameter() {
        assert_eq!(learned_mode(&ParamKind::Toggle), Mode::Toggle);
        assert_eq!(learned_mode(&ParamKind::Pulse), Mode::Momentary);
        assert_eq!(
            learned_mode(&ParamKind::Float {
                min: None,
                max: None
            }),
            Mode::Absolute
        );
    }

    #[test]
    fn labels_name_the_node() {
        let t = Target {
            node: NodeId(4),
            param: "colour".into(),
            component: Some(2),
        };
        assert_eq!(target_label(None, &t), "n4.colour[2]");
        assert_eq!(
            Source::Midi {
                control: MidiControl::Cc(7),
                channel: Some(1),
                device: String::new()
            }
            .label(),
            "CC 7 ch1"
        );
    }

    fn add_constant(e: &mut Engine, name: &str) -> NodeId {
        let id = e.alloc_id();
        e.edit(
            "add",
            [Edit::AddNode {
                node: atd_core::graph::Node::new(id, "img.constant", name),
            }],
        )
        .unwrap();
        id
    }

    fn osc_map(maps: &mut Mappings, address: &str, node: NodeId, component: usize) -> u64 {
        maps.add(
            Source::Osc {
                address: address.into(),
                arg: 0,
            },
            Target {
                node,
                param: "colour".into(),
                component: Some(component),
            },
            [0.0, 1.0],
        )
    }

    /// One frame of the mappings, after `x` arrived on `address`.
    fn osc(c: &mut Control, e: &mut Engine, maps: &Mappings, address: &str, x: f64) {
        // The hub matches against the mappings the last update synced.
        c.apply(e, maps);
        c.rt.hub().push_osc(address, &[x]);
        c.apply(e, maps);
    }

    fn colour(e: &Engine, id: NodeId) -> serde_json::Value {
        e.network()
            .param_value(e.registry(), id, "colour")
            .unwrap()
            .clone()
    }

    /// Saving stores what the mapping wrote, only in the component it
    /// drives (not a timeline's or OSCQuery's live value), and only once
    /// the files were written.
    #[test]
    fn saving_stores_only_what_mappings_wrote() {
        use serde_json::json;
        let Some(mut e) = crate::project::tests::engine() else {
            return;
        };
        let id = add_constant(&mut e, "constant1");
        let stored = colour(&e, id);
        let s = stored.as_array().unwrap().clone();
        let mut state = crate::project::ProjectState::untitled(e.network());
        state.meta.name = "Mapped".into();
        osc_map(&mut state.meta.mappings, "/red", id, 0);
        let maps = state.meta.mappings.clone();
        let mut c = Control::new(false);
        osc(&mut c, &mut e, &maps, "/red", 0.5);
        assert_eq!(
            e.live_values().get(id, "colour"),
            Some(&json!([0.5, s[1], s[2], s[3]]))
        );
        // Another writer (the timeline, OSCQuery) moves component 1.
        e.set_live(id, "colour", Some(&json!([0.5, 0.9, s[2], s[3]])));

        // A save that fails edits nothing.
        let blocker = std::env::temp_dir().join(format!("atd-app-blocker-{}", std::process::id()));
        std::fs::write(&blocker, "file").unwrap();
        let before = e.network().clone();
        let store = c.stored_values(&e);
        assert_eq!(store.len(), 1, "one parameter to store");
        state.save_to(&mut e, &blocker.join("x.atidarla"), store);
        assert_eq!(state.message.as_ref().map(|m| m.1), Some(true));
        assert_eq!(e.network(), &before, "no stored-value edit");
        std::fs::remove_file(&blocker).ok();

        let dir =
            std::env::temp_dir().join(format!("atd-app-mapped-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = c.stored_values(&e);
        state.save_to(&mut e, &dir, store);
        assert_eq!(
            state.message.as_ref().map(|m| m.1),
            Some(false),
            "{:?}",
            state.message
        );
        assert!(!state.dirty(&e));
        let want = json!([0.5, s[1], s[2], s[3]]);
        assert_eq!(colour(&e, id), want, "the patch has it");
        let project = atd_format::load(&dir).unwrap();
        assert_eq!(project.mappings, state.meta.mappings);
        let node = project.main().unwrap().node(id).unwrap();
        assert_eq!(node.params["colour"].value, want, "the file has it");
        assert!(
            e.live_values().get(id, "colour").is_some(),
            "still live after saving"
        );
        // Nothing written by a mapping: nothing to store.
        assert!(Control::new(false).stored_values(&e).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A deleted or retargeted mapping releases its parameter (or it
    /// would stay frozen), unless another mapping still drives it or
    /// another writer has set it since.
    #[test]
    fn removing_a_mapping_releases_its_parameter() {
        use serde_json::json;
        let Some(mut e) = crate::project::tests::engine() else {
            return;
        };
        let a = add_constant(&mut e, "constant1");
        let b = add_constant(&mut e, "constant2");
        let mut maps = Mappings::default();
        let red = osc_map(&mut maps, "/red", a, 0);
        let green = osc_map(&mut maps, "/green", a, 1);
        let mut c = Control::new(false);
        osc(&mut c, &mut e, &maps, "/red", 0.5);
        osc(&mut c, &mut e, &maps, "/green", 0.25);
        assert!(e.live_values().get(a, "colour").is_some());

        maps.remove(green);
        c.apply(&mut e, &maps);
        assert!(
            e.live_values().get(a, "colour").is_some(),
            "another mapping still drives it"
        );

        // Retargeted to another node: the first one is released.
        maps.mappings[0].target.node = b;
        c.apply(&mut e, &maps);
        assert_eq!(e.live_values().get(a, "colour"), None, "retargeted");
        osc(&mut c, &mut e, &maps, "/red", 0.75);
        assert!(e.live_values().get(b, "colour").is_some());

        // Another writer set it since: deleting the mapping leaves that.
        let theirs = json!([0.1, 0.2, 0.3, 1.0]);
        e.set_live(b, "colour", Some(&theirs));
        maps.remove(red);
        c.apply(&mut e, &maps);
        assert_eq!(e.live_values().get(b, "colour"), Some(&theirs));

        // Deleted with its value still live: released.
        let blue = osc_map(&mut maps, "/blue", a, 2);
        osc(&mut c, &mut e, &maps, "/blue", 1.0);
        assert!(e.live_values().get(a, "colour").is_some());
        maps.remove(blue);
        c.apply(&mut e, &maps);
        assert_eq!(e.live_values().get(a, "colour"), None, "deleted");
    }
}
