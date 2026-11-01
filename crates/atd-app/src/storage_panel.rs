//! The Storage section of the AI Hosts window (PRD A70 c): what the app
//! keeps for this user, with sizes, a Remove button per kind, and Remove
//! app data…, the uninstall of macOS and Linux (Windows uninstalls from
//! Settings > Apps). Listing walks folders and removing deletes them, so
//! both run on workers ([`atd_ai::storage`]); the rows' text is built once
//! per listing.

use atd_ai::models::Job;
use atd_ai::storage::{self, Entry, Kind, Removal, Storage, human_size};
use egui::Ui;

/// What the caller must do about a removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    None,
    /// The AI Host packages are about to go: stop the local host first.
    PackagesGoing,
}

#[derive(Debug)]
struct Row {
    kind: Kind,
    label: &'static str,
    size: String,
    /// What it holds, and where (hover).
    items: String,
    paths: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    /// Remove one kind: the question, what it removes, the button.
    Kind {
        kind: Kind,
        heading: String,
        body: String,
        button: String,
    },
    /// Remove app data: the explanation of the models kept or removed.
    AppData(String),
}

#[derive(Debug, Default)]
pub struct StoragePanel {
    storage: Option<Storage>,
    listing: Option<Job<Vec<Entry>>>,
    /// A listing has been asked for since the window opened.
    listed: bool,
    rows: Vec<Row>,
    models: Vec<Entry>,
    removing: Option<(Job<Removal>, Vec<Kind>, bool)>,
    confirm: Option<Confirm>,
    /// Remove app data also removes the model files.
    with_models: bool,
    /// The last result: (text, is an error).
    note: Option<(String, bool)>,
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Packages => "AI Host packages",
        Kind::Models => "Models",
        Kind::Cache => "Caches and logs",
        Kind::Settings => "Settings and keys",
    }
}

impl StoragePanel {
    fn storage(&mut self) -> Storage {
        self.storage.get_or_insert_with(Storage::user).clone()
    }

    fn relist(&mut self) {
        self.listing = Some(storage::start_list(self.storage()));
        self.listed = true;
    }

    /// The window closed: list again when it opens.
    pub fn closed(&mut self) {
        self.listed = false;
    }

    fn rows_from(&mut self, entries: Vec<Entry>) {
        self.rows.clear();
        for kind in Kind::ALL {
            let of: Vec<&Entry> = entries.iter().filter(|e| e.kind == kind).collect();
            if of.is_empty() {
                continue;
            }
            let what: Vec<&str> = of.iter().map(|e| e.what.as_str()).collect();
            let paths: Vec<String> = of.iter().map(|e| e.path.display().to_string()).collect();
            self.rows.push(Row {
                kind,
                label: label(kind),
                size: human_size(of.iter().map(|e| e.size).sum()),
                items: what.join(", "),
                paths: paths.join("\n"),
            });
        }
        self.models = entries
            .into_iter()
            .filter(|e| e.kind == Kind::Models)
            .collect();
    }

    fn remove(&mut self, kinds: Vec<Kind>, app_data: bool) -> Event {
        let s = self.storage();
        let k = kinds.clone();
        let packages = kinds.contains(&Kind::Packages);
        self.removing = Some((
            Job::spawn("atd-storage-remove", move |_| {
                if packages {
                    // Give the stopped host a moment to exit and let go of
                    // its files (Windows will not delete a loaded DLL).
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                s.remove(&k)
            }),
            kinds,
            app_data,
        ));
        if packages {
            Event::PackagesGoing
        } else {
            Event::None
        }
    }

    /// A removal is running (the AI assist switch waits for it).
    pub fn busy(&self) -> bool {
        self.removing.is_some()
    }

    /// Takes finished jobs' results.
    fn poll(&mut self) {
        if let Some(job) = &mut self.listing
            && let Some(done) = job.try_take()
        {
            self.listing = None;
            match done {
                Ok(entries) => self.rows_from(entries),
                Err(_) => self.note = Some(("Could not list the storage.".into(), true)),
            }
        }
        let Some((job, _, _)) = &mut self.removing else {
            return;
        };
        let Some(done) = job.try_take() else {
            return;
        };
        let (_, _, app_data) = self.removing.take().expect("removing");
        let removal = done.unwrap_or_default();
        self.note = Some(removal_note(&removal, app_data, self.models_size()));
        self.relist();
    }

    fn models_size(&self) -> u64 {
        self.models.iter().map(|e| e.size).sum()
    }

    /// The section. `packages_busy`: an install is running, so the packages
    /// cannot be removed now.
    pub fn show(&mut self, ui: &mut Ui, packages_busy: bool) -> Event {
        if !self.listed {
            self.relist();
        }
        self.poll();
        let busy = self.busy();
        if self.rows.is_empty() {
            ui.weak(if self.listing.is_some() {
                "Measuring…"
            } else {
                "Atidarla keeps nothing for this user."
            });
        }
        let mut remove = None;
        egui::Grid::new("atd_storage")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for row in &self.rows {
                    ui.label(row.label);
                    ui.monospace(&row.size);
                    ui.weak(&row.items).on_hover_text(&row.paths);
                    let blocked = busy || (row.kind == Kind::Packages && packages_busy);
                    let button = ui.add_enabled(!blocked, egui::Button::new("Remove").small());
                    // "Remove" alone does not say which row to a screen reader.
                    let name = format!("Remove {}", row.label);
                    button.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Button, !blocked, &name)
                    });
                    if button.clicked() {
                        remove = Some(row.kind);
                    }
                    ui.end_row();
                }
            });
        if let Some(kind) = remove
            && let Some(row) = self.rows.iter().find(|r| r.kind == kind)
        {
            let (heading, body, button) =
                confirm_text(row, &storage::models_explanation(&self.models));
            self.confirm = Some(Confirm::Kind {
                kind,
                heading,
                body,
                button,
            });
        }
        ui.horizontal(|ui| {
            if cfg!(windows) {
                ui.weak(
                    "To uninstall Atidarla, use Settings > Apps: it removes this too and asks \
                     about the model files.",
                );
            } else if ui
                .add_enabled(
                    !busy && !packages_busy,
                    egui::Button::new("Remove app data…"),
                )
                .on_hover_text("Before moving Atidarla to the Trash: remove what it keeps")
                .clicked()
            {
                self.with_models = false;
                self.confirm = Some(Confirm::AppData(storage::models_explanation(&self.models)));
            }
            if busy {
                ui.spinner();
                let status = ui.weak("Removing…");
                // Announced when it appears, not only when focused.
                ui.ctx().accesskit_node_builder(status.id, |node| {
                    node.set_role(egui::Role::Status);
                    node.set_live(egui::accesskit::Live::Polite);
                });
            }
        });
        if let Some((text, error)) = &self.note {
            let colour = if *error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.colored_label(colour, text);
        }
        Event::None
    }

    /// The confirmations, drawn over everything.
    pub fn modals(&mut self, ctx: &egui::Context) -> Event {
        let Some(confirm) = &self.confirm else {
            return Event::None;
        };
        let mut answer = None;
        let has_models = !self.models.is_empty();
        let modal = egui::Modal::new(egui::Id::unique("atd_storage_confirm")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            match confirm {
                Confirm::Kind {
                    kind,
                    heading,
                    body,
                    button,
                } => {
                    ui.heading(heading);
                    ui.label(body);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(button).clicked() {
                            answer = Some((vec![*kind], false));
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some((vec![], false));
                        }
                    });
                }
                Confirm::AppData(why) => {
                    ui.heading("Remove app data?");
                    ui.label(
                        "This removes what Atidarla keeps for you: AI Host packages, caches, \
                         logs, settings, host tokens and cloud keys. Projects are not touched.",
                    );
                    if has_models {
                        ui.add_space(4.0);
                        ui.label(why);
                        ui.checkbox(&mut self.with_models, "Also remove the model files");
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            let mut kinds = vec![Kind::Packages, Kind::Cache, Kind::Settings];
                            if self.with_models {
                                kinds.push(Kind::Models);
                            }
                            answer = Some((kinds, true));
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some((vec![], false));
                        }
                    });
                }
            }
        });
        if modal.should_close() && answer.is_none() {
            answer = Some((vec![], false));
        }
        let Some((kinds, app_data)) = answer else {
            return Event::None;
        };
        self.confirm = None;
        if kinds.is_empty() {
            return Event::None;
        }
        self.remove(kinds, app_data)
    }
}

/// The confirmation for removing one row's kind: its heading, its text
/// (what goes, its size, and what that means), and the button.
fn confirm_text(row: &Row, models_why: &str) -> (String, String, String) {
    let lead = format!("This removes {} ({}).", row.items, row.size);
    let (heading, more, button) = match row.kind {
        Kind::Packages => (
            "Remove the AI Host packages?",
            "AI nodes stop using the local AI Host. Turn on AI assist to reinstall them.",
            "Remove the packages",
        ),
        Kind::Models => (
            "Remove the model files?",
            models_why,
            "Remove the model files",
        ),
        Kind::Cache => (
            "Remove the caches and logs?",
            "They are made again when needed.",
            "Remove the caches",
        ),
        Kind::Settings => (
            "Remove the settings and keys?",
            "Your cloud keys and host tokens are removed too: enter them again to use those \
             hosts. Atidarla writes no settings again until it restarts.",
            "Remove the settings",
        ),
    };
    (
        heading.to_owned(),
        format!("{lead} {more}"),
        button.to_owned(),
    )
}

/// What a removal tells the user: what was freed, what failed, and after
/// Remove app data what to do next.
fn removal_note(r: &Removal, app_data: bool, models_kept: u64) -> (String, bool) {
    if let Some(f) = r.failed.first() {
        return (
            format!(
                "{f}. Close anything using it and try again ({} failed).",
                r.failed.len()
            ),
            true,
        );
    }
    let mut text = format!(
        "Removed {}, freeing {}.",
        r.removed.len(),
        human_size(r.freed())
    );
    let models_removed = r.removed.iter().any(|e| e.kind == Kind::Models);
    if app_data {
        if !models_removed && models_kept > 0 {
            text.push_str(&format!(
                " The model files ({}) are kept.",
                human_size(models_kept)
            ));
        }
        text.push_str(if cfg!(target_os = "macos") {
            " You can now drag Atidarla to the Trash."
        } else {
            " You can now delete the Atidarla app."
        });
    }
    (text, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, what: &str, size: u64) -> Entry {
        Entry {
            kind,
            what: what.into(),
            path: format!("/u/{what}").into(),
            size,
        }
    }

    #[test]
    fn rows_group_by_kind_with_sizes() {
        let mut p = StoragePanel::default();
        p.rows_from(vec![
            entry(Kind::Packages, "AI Host packages", 2_500_000_000),
            entry(Kind::Models, "model files", 4_000_000_000),
            entry(Kind::Models, "licence acceptances", 300),
            entry(Kind::Settings, "host tokens", 20),
        ]);
        let rows: Vec<(&str, &str)> = p.rows.iter().map(|r| (r.label, r.size.as_str())).collect();
        assert_eq!(
            rows,
            [
                ("AI Host packages", "2.50 GB"),
                ("Models", "4.00 GB"),
                ("Settings and keys", "20 B"),
            ]
        );
        assert_eq!(p.rows[1].items, "model files, licence acceptances");
        assert_eq!(p.models.len(), 2);
    }

    #[test]
    fn every_kind_is_confirmed_with_what_goes_and_its_size() {
        let mut p = StoragePanel::default();
        p.rows_from(vec![
            entry(Kind::Packages, "AI Host packages", 2_500_000_000),
            entry(Kind::Models, "model files", 4_000_000_000),
            entry(Kind::Cache, "logs", 10),
            entry(Kind::Settings, "host tokens", 20),
            entry(Kind::Settings, "cloud keys", 30),
        ]);
        assert_eq!(p.rows.len(), 4);
        for row in &p.rows {
            let (heading, body, button) = confirm_text(row, "MODELS WHY");
            assert!(heading.ends_with('?') && button.starts_with("Remove"));
            assert!(
                body.contains(&row.items) && body.contains(&row.size),
                "{body}"
            );
            match row.kind {
                Kind::Packages => assert!(body.contains("AI assist to reinstall"), "{body}"),
                Kind::Models => assert!(body.contains("MODELS WHY"), "{body}"),
                Kind::Settings => {
                    assert!(body.contains("cloud keys and host tokens"), "{body}")
                }
                Kind::Cache => {}
            }
        }
    }

    #[test]
    fn remove_app_data_says_what_is_kept_and_what_to_do_next() {
        let r = Removal {
            removed: vec![entry(Kind::Packages, "AI Host packages", 1_000_000)],
            failed: vec![],
        };
        let (text, error) = removal_note(&r, true, 4_000_000_000);
        assert!(!error);
        assert!(text.contains("freeing 1.0 MB"), "{text}");
        assert!(text.contains("model files (4.00 GB) are kept"), "{text}");
        assert!(text.contains("You can now"), "{text}");
        let (text, _) = removal_note(&r, false, 4_000_000_000);
        assert!(!text.contains("You can now"), "{text}");
    }
}
