//! Minimal docking layout: four areas (left, right, bottom, centre), each a
//! tab group. Tabs are dragged between areas or reordered by dropping them on
//! another tab; side areas are resizable.
//!
//! This exists because `egui_dock` and `egui_tiles` do not yet support egui
//! 0.37 (PRD A5). The API mirrors `egui_dock`'s `DockState` + `TabViewer` so
//! the module can be swapped out once they catch up.

use egui::{Id, Sense, Ui};

/// A dock area. Order matters: side panels must be laid out before the centre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    Left,
    Right,
    Bottom,
    Center,
}

impl Area {
    pub const ALL: [Area; 4] = [Area::Left, Area::Right, Area::Bottom, Area::Center];

    const fn index(self) -> usize {
        self as usize
    }
}

/// Renders tab contents for a [`DockState`].
pub trait TabViewer {
    type Tab;
    fn title(&self, tab: &Self::Tab) -> String;
    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab);
}

/// Identifies a tab being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabRef {
    area: Area,
    index: usize,
}

/// A requested move, applied after the frame's layout is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Move {
    from: TabRef,
    to: Area,
    /// Insert before this index in the target area; `None` appends.
    before: Option<usize>,
}

/// Tabs per area plus the active tab of each area.
#[derive(Debug, Clone)]
pub struct DockState<Tab> {
    areas: [Vec<Tab>; 4],
    active: [usize; 4],
}

impl<Tab> DockState<Tab> {
    pub fn new(left: Vec<Tab>, right: Vec<Tab>, bottom: Vec<Tab>, center: Vec<Tab>) -> Self {
        Self {
            areas: [left, right, bottom, center],
            active: [0; 4],
        }
    }

    #[cfg(test)]
    pub fn tabs(&self, area: Area) -> &[Tab] {
        &self.areas[area.index()]
    }

    #[cfg(test)]
    pub fn active(&self, area: Area) -> Option<&Tab> {
        self.areas[area.index()].get(self.active[area.index()])
    }

    /// Makes the first tab matching `pred` active and returns whether one was found.
    pub fn focus(&mut self, mut pred: impl FnMut(&Tab) -> bool) -> bool {
        for area in Area::ALL {
            if let Some(i) = self.areas[area.index()].iter().position(&mut pred) {
                self.active[area.index()] = i;
                return true;
            }
        }
        false
    }

    /// Shows `tab`: makes it active where it is, or adds it to `area` (a
    /// panel that is closed by default).
    pub fn open(&mut self, area: Area, tab: Tab)
    where
        Tab: PartialEq,
    {
        if self.focus(|t| *t == tab) {
            return;
        }
        self.areas[area.index()].push(tab);
        self.active[area.index()] = self.areas[area.index()].len() - 1;
    }

    /// Removes the first tab matching `pred`, keeping its area's active
    /// index valid. Returns whether one was found.
    pub fn close(&mut self, mut pred: impl FnMut(&Tab) -> bool) -> bool {
        for area in Area::ALL {
            let tabs = &mut self.areas[area.index()];
            if let Some(i) = tabs.iter().position(&mut pred) {
                tabs.remove(i);
                let active = &mut self.active[area.index()];
                if *active > i {
                    *active -= 1;
                }
                *active = (*active).min(tabs.len().saturating_sub(1));
                return true;
            }
        }
        false
    }

    /// Moves a tab, keeping each area's active index valid. The moved tab
    /// becomes active in its new area.
    fn apply(&mut self, mv: Move) {
        let (from, to) = (mv.from.area.index(), mv.to.index());
        if mv.from.index >= self.areas[from].len() {
            return;
        }
        let tab = self.areas[from].remove(mv.from.index);
        let mut before = mv.before.unwrap_or(self.areas[to].len());
        if from == to && mv.from.index < before {
            before -= 1;
        }
        let before = before.min(self.areas[to].len());
        self.areas[to].insert(before, tab);
        self.active[to] = before;
        if from != to {
            let len = self.areas[from].len();
            self.active[from] = self.active[from].min(len.saturating_sub(1));
        }
    }

    /// Lays out all areas into `ui` (normally the window's root `Ui`).
    pub fn show(&mut self, ui: &mut Ui, viewer: &mut impl TabViewer<Tab = Tab>) {
        let mut pending: Option<Move> = None;
        let dragging = egui::DragAndDrop::has_payload_of_type::<TabRef>(ui.ctx());

        for area in Area::ALL {
            let empty = self.areas[area.index()].is_empty();
            if empty && !dragging && area != Area::Center {
                continue;
            }
            let mut contents = |ui: &mut Ui| {
                if let Some(mv) = self.area_ui(ui, area, viewer) {
                    pending = Some(mv);
                }
            };
            match area {
                Area::Left => {
                    egui::Panel::left("dock_left")
                        .resizable(true)
                        .default_size(260.0)
                        .show(ui, &mut contents);
                }
                Area::Right => {
                    egui::Panel::right("dock_right")
                        .resizable(true)
                        .default_size(300.0)
                        .show(ui, &mut contents);
                }
                Area::Bottom => {
                    egui::Panel::bottom("dock_bottom")
                        .resizable(true)
                        .default_size(220.0)
                        .show(ui, &mut contents);
                }
                Area::Center => {
                    egui::CentralPanel::default().show(ui, &mut contents);
                }
            }
        }

        if let Some(mv) = pending {
            self.apply(mv);
        }
    }

    fn area_ui(
        &mut self,
        ui: &mut Ui,
        area: Area,
        viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> Option<Move> {
        let mut pending = None;
        let area_rect = ui.max_rect();

        // Tab bar.
        ui.horizontal(|ui| {
            for index in 0..self.areas[area.index()].len() {
                let title = viewer.title(&self.areas[area.index()][index]);
                let selected = self.active[area.index()] == index;
                // One widget senses both, so a click selects and a drag moves.
                let response = ui
                    .add(egui::Button::selectable(selected, title).sense(Sense::click_and_drag()));
                if response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), TabRef { area, index });
                }
                if response.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                if response.clicked() {
                    self.active[area.index()] = index;
                }
                if let Some(from) = response.dnd_release_payload::<TabRef>() {
                    pending = Some(Move {
                        from: *from,
                        to: area,
                        before: Some(index),
                    });
                }
            }
        });
        ui.separator();

        // Active tab body.
        let active = self.active[area.index()];
        if let Some(tab) = self.areas[area.index()].get_mut(active) {
            viewer.ui(ui, tab);
        } else {
            ui.centered_and_justified(|ui| ui.weak("Drop a tab here"));
        }

        // Dropping anywhere else in the area appends.
        let drop = ui.interact(
            area_rect,
            Id::unique(("dock_area", area.index())),
            Sense::hover(),
        );
        if pending.is_none()
            && let Some(from) = drop.dnd_release_payload::<TabRef>()
        {
            pending = Some(Move {
                from: *from,
                to: area,
                before: None,
            });
        }
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> DockState<&'static str> {
        DockState::new(vec!["a", "b"], vec!["c"], vec![], vec!["d", "e", "f"])
    }

    fn mv(area: Area, index: usize, to: Area, before: Option<usize>) -> Move {
        Move {
            from: TabRef { area, index },
            to,
            before,
        }
    }

    #[test]
    fn move_between_areas_appends_and_activates() {
        let mut s = state();
        s.apply(mv(Area::Left, 1, Area::Bottom, None));
        assert_eq!(s.tabs(Area::Left), ["a"]);
        assert_eq!(s.tabs(Area::Bottom), ["b"]);
        assert_eq!(s.active(Area::Bottom), Some(&"b"));
    }

    #[test]
    fn reorder_within_area() {
        let mut s = state();
        s.apply(mv(Area::Center, 0, Area::Center, Some(2)));
        assert_eq!(s.tabs(Area::Center), ["e", "d", "f"]);
        assert_eq!(s.active(Area::Center), Some(&"d"));
        s.apply(mv(Area::Center, 2, Area::Center, Some(0)));
        assert_eq!(s.tabs(Area::Center), ["f", "e", "d"]);
    }

    #[test]
    fn emptying_an_area_keeps_active_index_valid() {
        let mut s = state();
        s.apply(mv(Area::Right, 0, Area::Left, Some(0)));
        assert!(s.tabs(Area::Right).is_empty());
        assert_eq!(s.active(Area::Right), None);
        assert_eq!(s.tabs(Area::Left), ["c", "a", "b"]);
    }

    #[test]
    fn stale_move_is_ignored() {
        let mut s = state();
        s.apply(mv(Area::Bottom, 3, Area::Left, None));
        assert_eq!(s.tabs(Area::Left), ["a", "b"]);
    }

    #[test]
    fn open_adds_a_closed_tab_or_focuses_an_open_one() {
        let mut s = state();
        s.open(Area::Bottom, "t");
        assert_eq!(s.tabs(Area::Bottom), ["t"]);
        assert_eq!(s.active(Area::Bottom), Some(&"t"));
        // Already open: focused where it is, not added again.
        s.open(Area::Bottom, "e");
        assert_eq!(s.tabs(Area::Bottom), ["t"]);
        assert_eq!(s.active(Area::Center), Some(&"e"));
    }

    #[test]
    fn close_removes_a_tab_and_keeps_active_valid() {
        let mut s = state();
        s.focus(|t| *t == "f");
        assert!(s.close(|t| *t == "d"));
        assert_eq!(s.tabs(Area::Center), ["e", "f"]);
        assert_eq!(s.active(Area::Center), Some(&"f"));
        assert!(s.close(|t| *t == "f"));
        assert_eq!(s.active(Area::Center), Some(&"e"));
        assert!(s.close(|t| *t == "c"));
        assert_eq!(s.active(Area::Right), None);
        assert!(!s.close(|t| *t == "zzz"));
    }

    #[test]
    fn focus_finds_tab() {
        let mut s = state();
        assert!(s.focus(|t| *t == "f"));
        assert_eq!(s.active(Area::Center), Some(&"f"));
        assert!(!s.focus(|t| *t == "zzz"));
    }
}
