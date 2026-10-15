//! The Patch view's node canvas (FR-P1). Owned by Atidarla rather than taken
//! from a node-graph crate (RESEARCH §5.1).
//!
//! **Status (M0):** empty canvas with pan, zoom-about-cursor and a grid.
//! Nodes, wires, selection, search-to-add and the minimap land in M1.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.0;
/// Grid spacing in canvas units.
const GRID: f32 = 32.0;
/// Every n-th grid line is drawn stronger.
const MAJOR_EVERY: i64 = 8;

/// Canvas-to-screen mapping: `screen = origin + offset + canvas * zoom`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub offset: Vec2,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            offset: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl View {
    pub fn to_screen(self, origin: Pos2, canvas: Pos2) -> Pos2 {
        origin + self.offset + canvas.to_vec2() * self.zoom
    }

    pub fn to_canvas(self, origin: Pos2, screen: Pos2) -> Pos2 {
        ((screen - origin - self.offset) / self.zoom).to_pos2()
    }

    /// Multiplies the zoom by `factor` (clamped), keeping the canvas point
    /// under `anchor` fixed on screen.
    pub fn zoom_about(&mut self, origin: Pos2, anchor: Pos2, factor: f32) {
        let before = self.to_canvas(origin, anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.offset = anchor - origin - before.to_vec2() * self.zoom;
    }
}

#[derive(Debug, Default)]
pub struct NodeCanvas {
    pub view: View,
}

impl NodeCanvas {
    pub fn ui(&mut self, ui: &mut Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let origin = rect.center();

        // Pan: middle-drag anywhere, or primary-drag on empty canvas.
        if response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Primary)
        {
            self.view.offset += response.drag_delta();
        }
        // Zoom: pinch / ctrl+scroll, or plain scroll wheel.
        if let Some(pointer) = response.hover_pos() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            let factor = if zoom != 1.0 {
                zoom
            } else {
                (scroll.y * 0.002).exp()
            };
            if factor != 1.0 {
                self.view.zoom_about(origin, pointer, factor);
            }
        }
        if response.double_clicked() {
            self.view = View::default();
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        self.paint_grid(&painter, rect, origin, ui.visuals().dark_mode);

        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Empty network — operators arrive in M1",
            egui::FontId::proportional(14.0),
            ui.visuals().weak_text_color(),
        );
        painter.text(
            rect.left_bottom() + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!(
                "{:.0}%  ·  drag to pan, scroll to zoom, double-click to reset",
                self.view.zoom * 100.0
            ),
            egui::FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
    }

    fn paint_grid(&self, painter: &egui::Painter, rect: Rect, origin: Pos2, dark: bool) {
        let step = GRID * self.view.zoom;
        if step < 6.0 {
            return;
        }
        let base = if dark {
            Color32::from_white_alpha(10)
        } else {
            Color32::from_black_alpha(14)
        };
        let major = if dark {
            Color32::from_white_alpha(24)
        } else {
            Color32::from_black_alpha(32)
        };
        let min = self.view.to_canvas(origin, rect.min);
        let max = self.view.to_canvas(origin, rect.max);

        let first_x = (min.x / GRID).floor() as i64;
        let last_x = (max.x / GRID).ceil() as i64;
        for i in first_x..=last_x {
            let x = self
                .view
                .to_screen(origin, Pos2::new(i as f32 * GRID, 0.0))
                .x;
            let colour = if i.rem_euclid(MAJOR_EVERY) == 0 {
                major
            } else {
                base
            };
            painter.vline(x, rect.y_range(), Stroke::new(1.0, colour));
        }
        let first_y = (min.y / GRID).floor() as i64;
        let last_y = (max.y / GRID).ceil() as i64;
        for i in first_y..=last_y {
            let y = self
                .view
                .to_screen(origin, Pos2::new(0.0, i as f32 * GRID))
                .y;
            let colour = if i.rem_euclid(MAJOR_EVERY) == 0 {
                major
            } else {
                base
            };
            painter.hline(rect.x_range(), y, Stroke::new(1.0, colour));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn screen_canvas_round_trip() {
        let view = View {
            offset: Vec2::new(13.0, -7.0),
            zoom: 1.75,
        };
        let origin = Pos2::new(400.0, 300.0);
        let p = Pos2::new(-21.5, 88.0);
        assert!(close(view.to_canvas(origin, view.to_screen(origin, p)), p));
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut view = View {
            offset: Vec2::new(40.0, 10.0),
            zoom: 1.0,
        };
        let origin = Pos2::new(500.0, 400.0);
        let anchor = Pos2::new(620.0, 333.0);
        let under = view.to_canvas(origin, anchor);
        view.zoom_about(origin, anchor, 2.5);
        assert_eq!(view.zoom, 2.5);
        assert!(close(view.to_screen(origin, under), anchor));
    }

    #[test]
    fn zoom_is_clamped() {
        let mut view = View::default();
        view.zoom_about(Pos2::ZERO, Pos2::ZERO, 1000.0);
        assert_eq!(view.zoom, MAX_ZOOM);
        view.zoom_about(Pos2::ZERO, Pos2::ZERO, 1e-6);
        assert_eq!(view.zoom, MIN_ZOOM);
    }
}
