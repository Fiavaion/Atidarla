//! Canvas geometry shared by the Patch editor (FR-P1): the canvas↔screen
//! view transform, pan/zoom input and the background grid. Atidarla owns its
//! node canvas rather than using a node-graph crate (RESEARCH §5.1).

use egui::{Color32, Pos2, Rect, Response, Stroke, Ui, Vec2};

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

    /// Pans on background drags (primary or middle) and zooms about the
    /// pointer on scroll or pinch while `background` is hovered.
    pub fn handle_input(&mut self, ui: &Ui, background: &Response, origin: Pos2) {
        if background.dragged_by(egui::PointerButton::Middle)
            || background.dragged_by(egui::PointerButton::Primary)
        {
            self.offset += background.drag_delta();
        }
        if let Some(pointer) = background.hover_pos() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            let factor = if zoom != 1.0 {
                zoom
            } else {
                (scroll.y * 0.002).exp()
            };
            if factor != 1.0 {
                self.zoom_about(origin, pointer, factor);
            }
        }
    }
}

/// Paints the background and grid.
pub fn paint_grid(ui: &Ui, painter: &egui::Painter, rect: Rect, origin: Pos2, view: View) {
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let step = GRID * view.zoom;
    if step < 6.0 {
        return;
    }
    let dark = ui.visuals().dark_mode;
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
    let min = view.to_canvas(origin, rect.min);
    let max = view.to_canvas(origin, rect.max);
    for i in (min.x / GRID).floor() as i64..=(max.x / GRID).ceil() as i64 {
        let x = view.to_screen(origin, Pos2::new(i as f32 * GRID, 0.0)).x;
        let colour = if i.rem_euclid(MAJOR_EVERY) == 0 {
            major
        } else {
            base
        };
        painter.vline(x, rect.y_range(), Stroke::new(1.0, colour));
    }
    for i in (min.y / GRID).floor() as i64..=(max.y / GRID).ceil() as i64 {
        let y = view.to_screen(origin, Pos2::new(0.0, i as f32 * GRID)).y;
        let colour = if i.rem_euclid(MAJOR_EVERY) == 0 {
            major
        } else {
            base
        };
        painter.hline(rect.x_range(), y, Stroke::new(1.0, colour));
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
