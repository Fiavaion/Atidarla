//! View > Accessibility: the UI scale, wire type labels, the wire legend
//! (colour, socket shape and label of each type) and the keyboard keys.
//! The settings are per user (`crate::settings`); docs/accessibility.md
//! has the same key list.

use egui::gui_zoom::kb_shortcuts;
use egui::{Context, Sense, Vec2};

use crate::settings::UiSettings;

/// Keys for using the app without a mouse, as (keys, what they do).
pub const KEYS: &[(&str, &str)] = &[
    (
        "Tab / Shift+Tab",
        "Move keyboard focus to the next / previous control",
    ),
    (
        "F6 / Shift+F6",
        "Move focus to the next / previous panel's tab bar",
    ),
    ("Arrows (in a panel)", "Move focus between nearby controls"),
    ("Enter or Space", "Press the focused button or tab"),
    (
        "Ctrl/Cmd + / - / 0",
        "Make the interface larger / smaller / 100%",
    ),
    ("Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z", "Undo, redo"),
    ("Canvas: arrows", "Select the nearest node that way"),
    (
        "Canvas: Shift+arrows",
        "Move the selection (with Ctrl/Cmd: further)",
    ),
    (
        "Canvas: W",
        "Wire from the selected node (W again: next output)",
    ),
    (
        "Canvas: W, arrows, I, Enter",
        "Choose the node, its input, connect",
    ),
    (
        "Canvas: Esc",
        "Cancel the wire (otherwise: leave the canvas)",
    ),
    ("Canvas: N", "Add an operator at the centre"),
    ("Canvas: Delete / Backspace", "Delete the selection"),
    ("Canvas: Ctrl/Cmd+A, Ctrl/Cmd+D", "Select all, duplicate"),
    ("Canvas: Ctrl/Cmd+C, X, V", "Copy, cut, paste"),
    ("Canvas: C", "Put the selection in a comment box"),
    ("Canvas: F", "Frame the whole patch"),
];

/// The window; `open` is the View menu's toggle.
pub fn window(ctx: &Context, open: &mut bool, settings: &mut UiSettings) {
    egui::Window::new("Accessibility")
        .open(open)
        .resizable(true)
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.heading("Interface scale");
            ui.horizontal(|ui| {
                let shortcut = |s: &egui::KeyboardShortcut| ui.ctx().format_shortcut(s);
                let (minus, plus, reset) = (
                    shortcut(&kb_shortcuts::ZOOM_OUT),
                    shortcut(&kb_shortcuts::ZOOM_IN),
                    shortcut(&kb_shortcuts::ZOOM_RESET),
                );
                if ui.button("Smaller").on_hover_text(minus).clicked() {
                    egui::gui_zoom::zoom_out(ui.ctx());
                }
                ui.label(format!("{:.0}%", ui.ctx().zoom_factor() * 100.0));
                if ui.button("Larger").on_hover_text(plus).clicked() {
                    egui::gui_zoom::zoom_in(ui.ctx());
                }
                if ui.button("100%").on_hover_text(reset).clicked() {
                    ui.ctx().set_zoom_factor(1.0);
                }
            });
            ui.weak("Kept for your user account, for every project.");
            ui.add_space(8.0);

            ui.heading("Wires");
            if ui
                .checkbox(&mut settings.wire_labels, "Label every wire with its type")
                .changed()
            {
                settings.save();
            }
            ui.weak(
                "Wire colours are chosen to stay distinct with red-green and blue-yellow \
                 colour blindness; each type also has its own socket shape.",
            );
            egui::Grid::new("wire_legend").striped(true).show(ui, |ui| {
                for w in crate::wires::ALL {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(40.0, 14.0), Sense::hover());
                    let p = ui.painter();
                    p.line_segment(
                        [
                            rect.left_center(),
                            rect.right_center() - Vec2::new(8.0, 0.0),
                        ],
                        egui::Stroke::new(2.5, crate::wires::colour(w)),
                    );
                    p.add(crate::wires::socket(
                        rect.right_center() - Vec2::new(5.0, 0.0),
                        5.0,
                        w,
                    ));
                    ui.monospace(crate::wires::tag(w));
                    ui.label(crate::wires::shape_name(w));
                    ui.end_row();
                }
            });
            ui.add_space(8.0);

            ui.heading("Keyboard");
            egui::Grid::new("keys").striped(true).show(ui, |ui| {
                for (keys, what) in KEYS {
                    ui.monospace(*keys);
                    ui.label(*what);
                    ui.end_row();
                }
            });
        });
}
