//! The viewer for DAT values: text as monospace lines, a table as a grid.

use atd_gpu::engine::Dat;
use egui::{RichText, Ui};

/// Rows drawn at most (the rest is counted, not shown).
const MAX_ROWS: usize = 500;

/// A one-line description of `dat`: its form and size.
pub fn describe(dat: &Dat) -> String {
    if dat.is_text() {
        let text = dat.as_text();
        format!("text, {} lines, {} bytes", dat.rows(), text.len())
    } else {
        format!("table, {} × {}", dat.rows(), dat.cols())
    }
}

pub fn ui(ui: &mut Ui, name: &str, dat: &Dat, error: Option<&str>, status: Option<&str>) {
    ui.horizontal(|ui| {
        ui.strong(name);
        ui.weak(describe(dat));
        if let Some(s) = status {
            ui.weak(s);
        }
        if let Some(e) = error {
            ui.colored_label(egui::Color32::from_rgb(230, 80, 80), e);
        }
    });
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if dat.is_text() {
                let text = dat.as_text();
                if text.is_empty() {
                    ui.weak("(empty)");
                }
                for line in atd_core::dat::lines(&text).take(MAX_ROWS) {
                    ui.label(RichText::new(line).monospace());
                }
            } else {
                let table = dat.as_table();
                egui::Grid::new("atd-dat-view")
                    .striped(true)
                    .show(ui, |ui| {
                        for row in table.iter_rows().take(MAX_ROWS) {
                            for cell in row {
                                ui.label(RichText::new(cell).monospace());
                            }
                            ui.end_row();
                        }
                    });
            }
            let rows = dat.rows();
            if rows > MAX_ROWS {
                ui.weak(format!("… {} more rows", rows - MAX_ROWS));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_gpu::engine::Table;

    #[test]
    fn values_are_described_by_form_and_size() {
        assert_eq!(describe(&Dat::text("a\nbc")), "text, 2 lines, 4 bytes");
        let t = Dat::table(Table::from_rows([["a", "b"], ["c", "d"], ["e", "f"]]));
        assert_eq!(describe(&t), "table, 3 × 2");
    }
}
