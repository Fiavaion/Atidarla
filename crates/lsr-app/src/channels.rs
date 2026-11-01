//! Channel viewer (FR-P3): current values and a rolling plot of the viewed
//! SIG node's channels.

use std::collections::VecDeque;

use egui::{Color32, Id, Pos2, Rect, Stroke, Ui, Vec2};
use lsr_core::graph::{ChannelRef, NodeId};
use lsr_gpu::engine::Channels;

/// Frames of history kept per channel.
const HISTORY: usize = 300;

const COLOURS: [Color32; 6] = [
    Color32::from_rgb(110, 210, 140),
    Color32::from_rgb(240, 170, 80),
    Color32::from_rgb(110, 170, 255),
    Color32::from_rgb(240, 110, 150),
    Color32::from_rgb(200, 200, 110),
    Color32::from_rgb(170, 130, 255),
];

#[derive(Debug, Default)]
pub struct ChannelHistory {
    node: Option<NodeId>,
    names: Vec<String>,
    values: Vec<VecDeque<f32>>,
}

impl ChannelHistory {
    /// Appends the current frame; resets when the node or channel set changes.
    pub fn push(&mut self, node: NodeId, ch: &Channels) {
        let same = self.node == Some(node)
            && self.names.len() == ch.len()
            && self.names.iter().zip(ch.names()).all(|(a, b)| a == b);
        if !same {
            self.node = Some(node);
            self.names = ch.names().map(str::to_owned).collect();
            self.values = vec![VecDeque::with_capacity(HISTORY); ch.len()];
        }
        for (c, hist) in self.values.iter_mut().enumerate() {
            if hist.len() == HISTORY {
                hist.pop_front();
            }
            hist.push_back(ch.last(c));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.iter().all(VecDeque::is_empty)
    }

    pub fn ui(&self, ui: &mut Ui, ch: &Channels) {
        egui::Grid::new("lsr-channels")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for c in 0..ch.len() {
                    // Drag a channel onto a parameter to bind it (FR-P2).
                    let payload = self.node.map(|node| ChannelRef {
                        node,
                        channel: ch.name(c).to_owned(),
                    });
                    match payload {
                        Some(payload) => {
                            ui.dnd_drag_source(Id::unique(("lsr-chan", c)), payload, |ui| {
                                ui.colored_label(COLOURS[c % COLOURS.len()], ch.name(c));
                            })
                            .response
                            .on_hover_text("Drag onto a parameter to bind it");
                        }
                        None => {
                            ui.colored_label(COLOURS[c % COLOURS.len()], ch.name(c));
                        }
                    }
                    ui.monospace(format!("{:>10.4}", ch.last(c)));
                    ui.end_row();
                }
            });
        if ch.is_empty() {
            ui.weak("No channels.");
            return;
        }
        let (lo, hi) = self
            .values
            .iter()
            .flatten()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(*v), hi.max(*v))
            });
        let (lo, hi) = if lo.is_finite() && hi > lo {
            (lo, hi)
        } else {
            (lo - 1.0, lo + 1.0)
        };
        let size = Vec2::new(
            ui.available_width(),
            ui.available_height().clamp(80.0, 300.0),
        );
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        let y = |v: f32| rect.bottom() - (v - lo) / (hi - lo) * rect.height();
        if lo < 0.0 && hi > 0.0 {
            painter.hline(
                rect.x_range(),
                y(0.0),
                Stroke::new(1.0, ui.visuals().weak_text_color()),
            );
        }
        for (c, hist) in self.values.iter().enumerate() {
            let pts: Vec<Pos2> = hist
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    Pos2::new(
                        rect.left() + i as f32 / (HISTORY - 1) as f32 * rect.width(),
                        y(*v),
                    )
                })
                .collect();
            painter.add(egui::Shape::line(
                pts,
                Stroke::new(1.5, COLOURS[c % COLOURS.len()]),
            ));
        }
        painter.text(
            Rect::from_min_size(rect.min, Vec2::ZERO).min + Vec2::new(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!("{hi:.3}"),
            egui::FontId::monospace(10.0),
            ui.visuals().weak_text_color(),
        );
        painter.text(
            rect.left_bottom() + Vec2::new(4.0, -2.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{lo:.3}"),
            egui::FontId::monospace(10.0),
            ui.visuals().weak_text_color(),
        );
    }
}
