//! Parameter panel generated from the operator schema (FR-P2).
//!
//! Every change goes through the engine's history. Continuous gestures
//! (dragging a value, typing) are coalesced into one undo step, which is
//! sealed as soon as no parameter widget is being dragged or focused.
//!
//! Number, toggle, vector and colour parameters can follow an expression
//! (`=` button, PRD A22) or a channel: the native subset runs every frame
//! on the cook path, anything else in Python a frame late. One number
//! drives every component of a vector (a colour's red, green and blue); a
//! Python tuple, or a binding to a whole SIG node with enough channels,
//! drives them one each (PRD A66). Python ones run only once the
//! project's Python is allowed (docs/python-ops.md); until then the panel
//! says so. Errors show at the top of the panel,
//! with the operator's status line if it has one (e.g. Share Out's
//! published name).

use atd_core::expr::ExprValue;
use atd_core::graph::{ChannelRef, Edit, NodeId, Param};
use atd_core::registry::WireType;
use atd_core::registry::{ParamKind, ParamSchema};
use atd_gpu::engine::Engine;
use egui::{DragValue, Ui};
use serde_json::{Value, json};

/// Shows the parameters of `node`; returns an error message if an edit failed.
/// Right-clicking a label learns a MIDI/OSC mapping (`control`).
pub fn ui(
    ui: &mut Ui,
    engine: &mut Engine,
    node: NodeId,
    control: &mut crate::mappings::Control,
) -> Option<String> {
    let Some(n) = engine.network().node(node) else {
        ui.weak("Select a node to edit its parameters.");
        return None;
    };
    let Some(schema) = engine.registry().get(&n.type_id).cloned() else {
        ui.weak(format!("Unknown operator type `{}`.", n.type_id));
        return None;
    };
    let name = n.name.clone();

    ui.horizontal(|ui| {
        ui.heading(&name);
        ui.weak(format!("{} · {}", schema.label, schema.type_id));
    });
    ui.label(&schema.summary);
    if let Some(e) = engine.error(node) {
        ui.colored_label(egui::Color32::from_rgb(230, 80, 80), e);
        if e.contains(atd_core::expr::PYTHON_NOT_ALLOWED) {
            ui.weak(
                "Python expressions are not evaluated until you allow this project's Python \
                 (File > Allow Python Operators…); until then the parameter keeps its stored \
                 value. Native expressions (time, frame, math, op()) still run.",
            );
        }
    }
    if let Some(status) = engine.status(node) {
        ui.label(status);
    }
    if let Some(st) = engine.stat(node) {
        ui.weak(format!(
            "cook {:.0} µs (avg {:.0} µs, budget {} µs) · {} cooks",
            st.last_us, st.avg_us, schema.cook_budget_us, st.cooks
        ));
    }
    ui.separator();

    let mut error = None;
    let mut active = false;
    egui::Grid::new(("atd-params", node.0))
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            for p in &schema.params {
                let current = engine
                    .network()
                    .param_value(engine.registry(), node, &p.name)
                    .cloned()
                    .unwrap_or_else(|| p.default.clone());
                let overridden = engine
                    .network()
                    .node(node)
                    .is_some_and(|n| n.params.contains_key(&p.name));
                let bindable = p.kind.can_follow();
                let label = ui.label(&p.label).on_hover_text(&p.help);
                control.param_label(&label, node, p);
                // Channel export by drag (FR-P2): drop a channel (from the
                // channel viewer, or a wire dragged out of a SIG node) here.
                let dropped = if bindable {
                    if label.dnd_hover_payload::<ChannelRef>().is_some() {
                        ui.painter().rect_stroke(
                            label.rect.expand(2.0),
                            3.0,
                            egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                            egui::StrokeKind::Outside,
                        );
                    }
                    label
                        .dnd_release_payload::<ChannelRef>()
                        .filter(|r| r.node != node)
                        .map(|r| (*r).clone())
                } else {
                    None
                };
                let bind = engine
                    .network()
                    .node(node)
                    .and_then(|n| n.params.get(&p.name))
                    .and_then(|p| p.bind.clone());
                let expr = engine
                    .network()
                    .node(node)
                    .and_then(|n| n.params.get(&p.name))
                    .and_then(|p| p.expr.clone());
                let mut set_expr: Option<Option<String>> = None;
                let new = match &bind {
                    _ if expr.is_some() => {
                        let live = engine.expression_value(node, &p.name);
                        let (changed, busy, toggled) =
                            expr_widget(ui, node, p, expr.as_deref().unwrap_or(""), live);
                        active |= busy;
                        if toggled {
                            set_expr = Some(None);
                        } else if let Some(text) = changed {
                            set_expr = Some(Some(text));
                        }
                        None
                    }
                    Some(b) => {
                        let src = engine
                            .network()
                            .node(b.node)
                            .map_or("?", |n| n.name.as_str());
                        let chan = if b.channel.is_empty() {
                            "first"
                        } else {
                            b.channel.as_str()
                        };
                        let shown = match &current {
                            // A vector or colour as its node reads it.
                            Value::Array(a) if p.kind.components().is_some() => Some(driven(
                                (0..a.len())
                                    .map(|i| engine.param_component(node, &p.name, i, true)),
                            )),
                            _ => engine
                                .channels(b.node)
                                .and_then(|c| c.value(&b.channel))
                                .map(|v| format!("{v:.3}")),
                        };
                        let hover = match p.kind.components() {
                            Some(_) if b.channel.is_empty() => format!(
                                "Follows {src}: one channel per component when it has \
                                 enough, else its first channel for every component"
                            ),
                            _ => format!("Follows {src}:{chan}"),
                        };
                        match shown {
                            Some(v) => ui.label(v),
                            None => ui.weak("—"),
                        }
                        .on_hover_text(hover);
                        None
                    }
                    None => {
                        let (new, busy) = ui
                            .horizontal(|ui| {
                                let out = widget(ui, p, &current);
                                if bindable
                                    && ui
                                        .small_button("=")
                                        .on_hover_text(
                                            "Follow an expression: time, frame, me.digits, \
                                             op('name').par.x, op('name')['channel'], maths, \
                                             or any Python",
                                        )
                                        .clicked()
                                {
                                    set_expr = Some(Some(expr_seed(&current)));
                                }
                                out
                            })
                            .inner;
                        active |= busy;
                        new
                    }
                };
                let rebind = bind_widget(ui, engine, node, p, bind.as_ref()).or(dropped.map(Some));
                let reset = ui
                    .add_enabled(overridden, egui::Button::new("reset").small())
                    .on_hover_text("Reset to default (and unbind)")
                    .clicked();
                ui.end_row();

                let edit = if let Some(expr) = set_expr {
                    Some(Edit::SetParam {
                        node,
                        param: p.name.clone(),
                        value: Some(Param {
                            value: current.clone(),
                            expr,
                            bind: None,
                        }),
                    })
                } else if reset {
                    Some(Edit::SetParam {
                        node,
                        param: p.name.clone(),
                        value: None,
                    })
                } else if let Some(rebind) = rebind {
                    let mut param = Param::constant(current.clone());
                    param.bind = rebind;
                    Some(Edit::SetParam {
                        node,
                        param: p.name.clone(),
                        value: Some(param),
                    })
                } else {
                    new.filter(|v| *v != current).map(|v| Edit::SetParam {
                        node,
                        param: p.name.clone(),
                        value: Some(Param::constant(v)),
                    })
                };
                if let Some(edit) = edit {
                    let key = format!("{node}:{}", p.name);
                    let label = format!("{name}.{}", p.name);
                    if let Err(e) = engine.edit_coalesced(&label, &key, [edit]) {
                        error = Some(e.to_string());
                    }
                }
            }
        });
    if schema.params.is_empty() {
        ui.weak("No parameters.");
    }
    if !active {
        engine.seal_history();
    }
    error
}

/// The starting text of a new expression: the current value (a vector's
/// first component, which the expression then gives every component).
fn expr_seed(current: &Value) -> String {
    match current {
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.first().map_or_else(|| "0".to_owned(), expr_seed),
        _ => "0".to_owned(),
    }
}

/// A driven vector's components for display: `(1.000, 0.500)`.
fn driven(components: impl Iterator<Item = Option<f64>>) -> String {
    let parts: Vec<String> = components
        .map(|c| c.map_or_else(|| "—".to_owned(), |v| format!("{v:.3}")))
        .collect();
    format!("({})", parts.join(", "))
}

/// An expression field. Returns the new text when committed (Enter or
/// leaving the field; Escape reverts), whether it is being edited, and
/// whether the `=` toggle was clicked to drop the expression.
fn expr_widget(
    ui: &mut Ui,
    node: NodeId,
    p: &ParamSchema,
    stored: &str,
    live: Option<ExprValue>,
) -> (Option<String>, bool, bool) {
    let id = egui::Id::unique(("atd-expr-draft", node.0, &p.name));
    let mut s = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| stored.to_owned());
    let mut toggled = false;
    let r = ui
        .horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut s)
                    .code_editor()
                    .desired_width(150.0),
            );
            match live {
                Some(v) => ui.weak(v.to_string()),
                None => ui.weak("—"),
            }
            .on_hover_text(if p.kind.components().is_some() {
                "The expression's current value: one number drives every component \
                 (a colour's red, green and blue); a Python tuple drives one each"
            } else {
                "The expression's current value"
            });
            toggled = ui
                .add(egui::Button::new("=").small().selected(true))
                .on_hover_text("Stop following the expression")
                .clicked();
            r
        })
        .inner;
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, s));
        return (None, true, toggled);
    }
    ui.data_mut(|d| d.remove::<String>(id));
    let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let commit = r.lost_focus() && !cancelled && s != stored && !s.trim().is_empty();
    (commit.then_some(s), false, toggled)
}

/// Whether a whole-node binding to SIG node `sig` drives a vector of
/// `components` one channel per component: the node has at least as many
/// channels (else its first channel drives every component; see
/// `atd_gpu::engine::Params::vec4`).
fn per_component(engine: &Engine, sig: NodeId, components: Option<usize>) -> bool {
    components.is_some_and(|n| n > 1 && engine.channels(sig).is_some_and(|c| c.len() >= n))
}

/// The channel-binding dropdown (FR-P2 channel export). Returns
/// `Some(new binding)` when the user changes it.
fn bind_widget(
    ui: &mut Ui,
    engine: &Engine,
    node: NodeId,
    p: &ParamSchema,
    current: Option<&ChannelRef>,
) -> Option<Option<ChannelRef>> {
    if !p.kind.can_follow() {
        ui.label("");
        return None;
    }
    let components = p.kind.components();
    let vector = components.is_some();
    let net = engine.network();
    let label = |r: &ChannelRef| {
        let n = net.node(r.node).map_or("?", |n| n.name.as_str());
        if r.channel.is_empty() && per_component(engine, r.node, components) {
            format!("{n} (per component)")
        } else if r.channel.is_empty() {
            n.to_owned()
        } else {
            format!("{n}:{}", r.channel)
        }
    };
    let mut options: Vec<ChannelRef> = Vec::new();
    for n in net.nodes() {
        let is_sig = engine
            .registry()
            .get(&n.type_id)
            .is_some_and(|s| s.outputs.iter().any(|o| o.wire == WireType::Sig));
        if !is_sig || n.id == node {
            continue;
        }
        let channels = engine.channels(n.id).filter(|c| !c.is_empty());
        // A vector can follow the whole node: channel i drives component i.
        if vector || channels.is_none() {
            options.push(ChannelRef {
                node: n.id,
                channel: String::new(),
            });
        }
        if let Some(c) = channels {
            options.extend(c.names().map(|ch| ChannelRef {
                node: n.id,
                channel: ch.to_owned(),
            }));
        }
    }
    let mut chosen = current.cloned();
    let text = current.map_or_else(|| "bind…".to_owned(), label);
    egui::ComboBox::from_id_salt(("atd-bind", node.0, &p.name))
        .selected_text(text)
        .width(110.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut chosen, None, "none");
            for o in &options {
                ui.selectable_value(&mut chosen, Some(o.clone()), label(o));
            }
            if options.is_empty() {
                ui.weak("Add a SIG operator to bind to");
            }
        })
        .response
        .on_hover_text("Drive this parameter from a SIG channel");
    (chosen.as_ref() != current).then_some(chosen)
}

/// Draws the widget for one parameter. Returns the new value if the user
/// changed it, and whether the widget is mid-gesture (dragged or focused).
fn widget(ui: &mut Ui, p: &ParamSchema, v: &Value) -> (Option<Value>, bool) {
    let busy = |r: &egui::Response| r.dragged() || r.has_focus();
    match &p.kind {
        ParamKind::Float { min, max } => {
            let mut x = v.as_f64().unwrap_or(0.0);
            let lo = min.unwrap_or(f64::NEG_INFINITY);
            let hi = max.unwrap_or(f64::INFINITY);
            let speed = if lo.is_finite() && hi.is_finite() {
                (hi - lo) / 300.0
            } else {
                0.01
            };
            let r = ui.add(
                DragValue::new(&mut x)
                    .speed(speed)
                    .range(lo..=hi)
                    .max_decimals(4),
            );
            (r.changed().then(|| json!(x)), busy(&r))
        }
        ParamKind::Int { min, max } => {
            let mut x = v.as_i64().unwrap_or(0);
            let r = ui.add(
                DragValue::new(&mut x)
                    .speed(0.1)
                    .range(min.unwrap_or(i64::MIN)..=max.unwrap_or(i64::MAX)),
            );
            (r.changed().then(|| json!(x)), busy(&r))
        }
        ParamKind::Toggle => {
            let mut b = v.as_bool().unwrap_or(false);
            let r = ui.checkbox(&mut b, "");
            (r.changed().then(|| json!(b)), false)
        }
        ParamKind::Pulse => {
            let r = ui.button("Pulse");
            // Pulses become momentary events with the event system (M1);
            // until then a click toggles the stored value.
            (
                r.clicked().then(|| json!(!v.as_bool().unwrap_or(false))),
                false,
            )
        }
        ParamKind::Menu { options } => {
            let mut sel = v.as_str().unwrap_or_default().to_owned();
            let before = sel.clone();
            let r = egui::ComboBox::from_id_salt(&p.name)
                .selected_text(&sel)
                .show_ui(ui, |ui| {
                    for o in options {
                        ui.selectable_value(&mut sel, o.clone(), o);
                    }
                });
            ((sel != before).then(|| json!(sel)), r.response.has_focus())
        }
        ParamKind::Colour => {
            let mut c = [0f32; 4];
            for (o, x) in c.iter_mut().zip(v.as_array().into_iter().flatten()) {
                *o = x.as_f64().unwrap_or(0.0) as f32;
            }
            let r = ui.color_edit_button_rgba_unmultiplied(&mut c);
            let popup_open = egui::Popup::is_any_open(ui.ctx());
            (
                r.changed()
                    .then(|| json!(c.map(|x| (x as f64 * 1e4).round() / 1e4))),
                popup_open,
            )
        }
        ParamKind::Vec { n } => {
            let mut xs: Vec<f64> = (0..usize::from(*n))
                .map(|i| v.get(i).and_then(Value::as_f64).unwrap_or(0.0))
                .collect();
            let mut changed = false;
            let mut active = false;
            ui.horizontal(|ui| {
                for x in &mut xs {
                    let r = ui.add(DragValue::new(x).speed(0.01).max_decimals(4));
                    changed |= r.changed();
                    active |= busy(&r);
                }
            });
            (changed.then(|| json!(xs)), active)
        }
        ParamKind::String | ParamKind::Path => {
            // Text commits on Enter or focus loss, not per keystroke (a file
            // path would otherwise be opened once per character); Escape
            // reverts. The draft lives in egui's memory while editing.
            let id = ui.scope_id().with(("atd-text-draft", &p.name));
            let stored = v.as_str().unwrap_or_default();
            let mut s = ui
                .data(|d| d.get_temp::<String>(id))
                .unwrap_or_else(|| stored.to_owned());
            let (r, picked) = ui
                .horizontal(|ui| {
                    let r = if p.multiline {
                        // Enter adds a line; Ctrl/Cmd+Enter or leaving commits.
                        ui.add(
                            egui::TextEdit::multiline(&mut s)
                                .code_editor()
                                .desired_rows(4)
                                .desired_width(260.0),
                        )
                    } else {
                        ui.add_sized([180.0, 18.0], egui::TextEdit::singleline(&mut s))
                    };
                    // Offered values (device names), listed in the background.
                    let mut picked = None;
                    if let Some(key) = &p.suggest {
                        ui.menu_button("…", |ui| {
                            ui.set_min_width(260.0);
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            let options = crate::suggest::list(key);
                            if options.is_empty() {
                                ui.weak("Looking…");
                            }
                            for o in options {
                                let r = ui.selectable_label(o == stored, &o);
                                if r.clicked() {
                                    picked = Some(o.clone());
                                    ui.close();
                                }
                                r.on_hover_text(o);
                            }
                        })
                        .response
                        .on_hover_text("Choose from the available devices");
                    }
                    (r, picked)
                })
                .inner;
            if let Some(o) = picked {
                ui.data_mut(|d| d.remove::<String>(id));
                return (Some(json!(o)), false);
            }
            let submit = p.multiline
                && r.has_focus()
                && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
            if submit {
                r.surrender_focus();
                ui.data_mut(|d| d.remove::<String>(id));
                return ((s != stored).then(|| json!(s)), false);
            }
            if r.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, s));
                (None, true)
            } else {
                ui.data_mut(|d| d.remove::<String>(id));
                let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let commit = r.lost_focus() && !cancelled && s != stored;
                (commit.then(|| json!(s)), false)
            }
        }
    }
}
