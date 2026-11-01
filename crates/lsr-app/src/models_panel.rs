//! The Models window (FR-A12, docs/models.md): the catalogue with licence
//! badges, the project's "commercial-safe only" setting, licence
//! acceptance, downloads, verification and removal.
//!
//! Disk and network work runs on model-manager threads ([`Job`]); the
//! window only polls them, so the patch keeps cooking (rule 1). Nothing
//! starts until the user opens the window and acts, so smoke runs are not
//! affected.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};
use lsr_ai::models::{
    self, Acceptances, DownloadError, DownloadOptions, Job, LicenceClass, Model, ModelPin, Status,
    Store, VerifyReport,
};
use lsr_format::AiSettings;

/// How often the window re-reads file sizes and acceptances while open.
const REFRESH: Duration = Duration::from_secs(2);

type Snapshot = (BTreeMap<String, Status>, Result<Acceptances, String>);

/// A model's last result, shown under its row: (text, is an error).
type Note = (String, bool);

#[derive(Debug, Default)]
pub struct ModelsPanel {
    pub open: bool,
    store: Store,
    statuses: BTreeMap<String, Status>,
    accepted: Acceptances,
    refreshed: Option<Instant>,
    refresh: Option<Job<Snapshot>>,
    downloads: BTreeMap<String, Job<Result<(), DownloadError>>>,
    verifies: BTreeMap<String, Job<VerifyReport>>,
    /// Accepting or removing, by model ID.
    writes: BTreeMap<String, Job<Result<String, String>>>,
    notes: BTreeMap<String, Note>,
    /// The acceptance dialog is open for this model.
    accepting: Option<String>,
    /// The remove confirmation is open for this model.
    removing: Option<String>,
    /// Sovereign mode blocked this model's download; offer a one-off.
    sovereign_blocked: Option<String>,
}

/// Badge colours from the Okabe–Ito palette (distinct under the common
/// colour-vision deficiencies); the text carries the meaning either way.
fn badge(class: LicenceClass) -> (Color32, &'static str) {
    match class {
        LicenceClass::CommercialSafe => (Color32::from_rgb(0, 158, 115), "COMMERCIAL-SAFE"),
        LicenceClass::Restricted => (Color32::from_rgb(230, 159, 0), "RESTRICTED"),
        LicenceClass::NonCommercial => (Color32::from_rgb(213, 94, 0), "NON-COMMERCIAL"),
    }
}

fn badge_ui(ui: &mut Ui, class: LicenceClass) {
    let (colour, text) = badge(class);
    egui::Frame::new()
        .fill(colour)
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(4, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(Color32::BLACK));
        })
        .response
        .on_hover_text(match class {
            LicenceClass::CommercialSafe => "Permissive licence: any use, commercial included.",
            LicenceClass::Restricted => {
                "Commercial use only under conditions (e.g. a revenue cap or registration). Read the licence."
            }
            LicenceClass::NonCommercial => "Research or personal use only.",
        });
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

/// Opens an https URL in the system browser, on its own thread.
fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    let url = url.to_owned();
    let _ = std::thread::Builder::new()
        .name("lsr-open-url".into())
        .spawn(move || {
            let result = if cfg!(target_os = "macos") {
                std::process::Command::new("open").arg(&url).status()
            } else if cfg!(windows) {
                std::process::Command::new("rundll32")
                    .args(["url.dll,FileProtocolHandler", &url])
                    .status()
            } else {
                std::process::Command::new("xdg-open").arg(&url).status()
            };
            if let Err(e) = result {
                log::warn!("cannot open {url}: {e}");
            }
        });
}

impl ModelsPanel {
    /// Draws the window when open, and collects finished jobs either way.
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        ai: &mut AiSettings,
        manifest: &BTreeMap<String, ModelPin>,
    ) {
        self.poll();
        if !self.open {
            return;
        }
        let due = self.refreshed.is_none_or(|t| t.elapsed() >= REFRESH);
        if due && self.refresh.is_none() {
            self.start_refresh();
        }
        let mut open = self.open;
        egui::Window::new("Models")
            .open(&mut open)
            .default_size([760.0, 520.0])
            .show(ctx, |ui| self.window_ui(ui, ai, manifest));
        self.open = open;
        self.dialogs(ctx);
        // Progress and status move while the window is open.
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn start_refresh(&mut self) {
        let store = self.store.clone();
        self.refresh = Some(Job::spawn("lsr-models-refresh", move |_| {
            let statuses = models::catalogue()
                .iter()
                .map(|m| (m.id.clone(), models::status(&store, m)))
                .collect();
            (statuses, Acceptances::load(store.accepted_path()))
        }));
    }

    /// Collects finished jobs.
    fn poll(&mut self) {
        if let Some(job) = &mut self.refresh
            && let Some(result) = job.try_take()
        {
            self.refresh = None;
            self.refreshed = Some(Instant::now());
            if let Ok((statuses, accepted)) = result {
                self.statuses = statuses;
                match accepted {
                    Ok(a) => self.accepted = a,
                    Err(e) => log::warn!("model licence acceptances: {e}"),
                }
            }
        }
        let mut changed = false;
        let ids: Vec<String> = self.downloads.keys().cloned().collect();
        for id in ids {
            let Some(result) = self.downloads.get_mut(&id).and_then(Job::try_take) else {
                continue;
            };
            self.downloads.remove(&id);
            changed = true;
            let note = match result {
                Ok(Ok(())) => ("Downloaded and checked.".to_owned(), false),
                Ok(Err(DownloadError::Sovereign { host })) => {
                    self.sovereign_blocked = Some(id.clone());
                    (
                        format!(
                            "Sovereign mode blocked {host}: it is on the internet. \
                             This project keeps AI traffic on this machine and the LAN."
                        ),
                        true,
                    )
                }
                Ok(Err(e)) => (e.to_string(), !matches!(e, DownloadError::Cancelled)),
                Err(_) => ("The download thread failed.".to_owned(), true),
            };
            self.notes.insert(id, note);
        }
        let ids: Vec<String> = self.verifies.keys().cloned().collect();
        for id in ids {
            let Some(result) = self.verifies.get_mut(&id).and_then(Job::try_take) else {
                continue;
            };
            self.verifies.remove(&id);
            changed = true;
            let note = match result {
                Ok(r) if r.cancelled => ("Check cancelled.".to_owned(), false),
                Ok(r) => {
                    let bad: Vec<&str> = r
                        .files
                        .iter()
                        .filter(|f| f.state != lsr_ai::models::FileState::Ok)
                        .map(|f| f.path.as_str())
                        .collect();
                    if bad.is_empty() {
                        (
                            format!("All {} files match their SHA-256.", r.files.len()),
                            false,
                        )
                    } else {
                        self.statuses.insert(id.clone(), r.status());
                        (
                            format!("{}: {}", r.status(), bad.join(", ")),
                            r.status() == Status::Corrupt,
                        )
                    }
                }
                Err(_) => ("The check failed.".to_owned(), true),
            };
            self.notes.insert(id, note);
        }
        let ids: Vec<String> = self.writes.keys().cloned().collect();
        for id in ids {
            let Some(result) = self.writes.get_mut(&id).and_then(Job::try_take) else {
                continue;
            };
            self.writes.remove(&id);
            changed = true;
            let note = match result {
                Ok(Ok(text)) => (text, false),
                Ok(Err(e)) => (e, true),
                Err(_) => ("Failed.".to_owned(), true),
            };
            self.notes.insert(id, note);
        }
        if changed {
            // Re-read sizes and acceptances now (unless a read is running).
            self.refreshed = None;
        }
    }

    fn busy(&self, id: &str) -> bool {
        self.downloads.contains_key(id)
            || self.verifies.contains_key(id)
            || self.writes.contains_key(id)
    }

    fn window_ui(
        &mut self,
        ui: &mut Ui,
        ai: &mut AiSettings,
        manifest: &BTreeMap<String, ModelPin>,
    ) {
        let cat = models::catalogue();
        ui.checkbox(
            &mut ai.commercial_safe_only,
            "Commercial-safe models only (this project)",
        )
        .on_hover_text(
            "Hide models whose licence is restricted or non-commercial, and make AI nodes refuse them. Saved in project.toml.",
        );
        let hidden = cat.len() - models::visible(cat, ai.commercial_safe_only).count();
        if hidden > 0 {
            ui.weak(format!(
                "{hidden} model(s) hidden: their licences are not commercial-safe."
            ));
        }
        let blocked = models::blocked(cat, ai.commercial_safe_only, manifest);
        if !blocked.is_empty() {
            ui.colored_label(
                Color32::from_rgb(213, 94, 0),
                format!(
                    "Warning: this project uses {}, which is not commercial-safe. AI nodes will refuse it.",
                    blocked.join(", ")
                ),
            );
        }
        let needed: Vec<String> = manifest
            .keys()
            .filter(|id| {
                cat.get(id).is_none() || self.statuses.get(*id).is_some_and(|s| *s != Status::Ready)
            })
            .cloned()
            .collect();
        if !needed.is_empty() {
            ui.label(format!(
                "This project needs models that are not on this machine: {}",
                needed.join(", ")
            ));
        }
        ui.horizontal(|ui| {
            ui.weak(format!("Cache: {}", self.store.root().display()));
            if lsr_ai::egress::sovereign() {
                ui.separator();
                ui.weak("Sovereign mode: on (downloads need a one-off permission)");
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("lsr-models-list")
            .show(ui, |ui| {
                for m in models::visible(cat, ai.commercial_safe_only) {
                    self.row(ui, m, manifest.contains_key(&m.id));
                    ui.separator();
                }
            });
    }

    fn row(&mut self, ui: &mut Ui, m: &'static Model, in_project: bool) {
        let status = self.statuses.get(&m.id).copied();
        let accepted = self.accepted.is_accepted(m);
        ui.horizontal(|ui| {
            badge_ui(ui, m.licence.class);
            ui.strong(&m.name).on_hover_text(format!(
                "{}\nAdapter: {}\nRuns: {}\nGPU memory: about {} MB\nSource: {} @ {}",
                m.task,
                m.adapter,
                m.platforms,
                m.vram_mb,
                m.source.repo,
                &m.source.revision[..12]
            ));
            ui.weak(format!("{} · {}", m.id, size_text(m.total_size())));
            if in_project {
                ui.label(RichText::new("used by this project").small());
            }
        });
        ui.horizontal(|ui| {
            ui.label(format!("Licence: {}", m.licence.id));
            if ui
                .button("Read licence")
                .on_hover_text(&m.licence.url)
                .clicked()
            {
                open_url(&m.licence.url);
            }
            if accepted {
                ui.weak("accepted");
            } else if ui
                .add_enabled(!self.busy(&m.id), egui::Button::new("Accept licence…"))
                .clicked()
            {
                self.accepting = Some(m.id.clone());
            }
        });
        ui.horizontal(|ui| {
            let text = match (status, self.downloads.get(&m.id)) {
                (_, Some(job)) => {
                    let p = job.progress();
                    ui.add(
                        egui::ProgressBar::new(p.fraction())
                            .desired_width(180.0)
                            .text(format!("{:.0}%", p.fraction() * 100.0)),
                    )
                    .on_hover_text(p.file);
                    None
                }
                (Some(s), None) => Some(s.to_string()),
                (None, None) => Some("…".to_owned()),
            };
            if let Some(t) = text {
                let colour = match status {
                    Some(Status::Corrupt) => Color32::from_rgb(213, 94, 0),
                    _ => ui.visuals().text_color(),
                };
                ui.label(RichText::new(format!("Status: {t}")).color(colour));
            }
            if let Some(job) = self.downloads.get(&m.id) {
                if ui.button("Cancel").clicked() {
                    job.cancel();
                }
            } else if status != Some(Status::Ready) {
                let resp = ui.add_enabled(
                    accepted && !self.busy(&m.id),
                    egui::Button::new(if status == Some(Status::Partial) {
                        "Resume download"
                    } else {
                        "Download"
                    }),
                );
                let resp = if accepted {
                    resp
                } else {
                    resp.on_disabled_hover_text("Accept the licence first.")
                };
                if resp.clicked() {
                    self.download(m, false);
                }
            }
            if let Some(job) = self.verifies.get(&m.id) {
                let p = job.progress();
                ui.add(
                    egui::ProgressBar::new(p.fraction())
                        .desired_width(120.0)
                        .text("checking"),
                );
                if ui.button("Stop check").clicked() {
                    job.cancel();
                }
            } else if ui
                .add_enabled(
                    !self.busy(&m.id) && matches!(status, Some(Status::Ready | Status::Corrupt)),
                    egui::Button::new("Verify"),
                )
                .on_hover_text("Check every file's SHA-256 against the catalogue")
                .clicked()
            {
                self.verifies.insert(
                    m.id.clone(),
                    models::start_verify(self.store.clone(), m.clone()),
                );
            }
            if ui
                .add_enabled(
                    !self.busy(&m.id) && status.is_some_and(|s| s != Status::NotDownloaded),
                    egui::Button::new("Remove…"),
                )
                .clicked()
            {
                self.removing = Some(m.id.clone());
            }
        });
        if self.sovereign_blocked.as_deref() == Some(m.id.as_str()) {
            ui.horizontal(|ui| {
                if ui
                    .button("Download once anyway")
                    .on_hover_text(
                        "Allow this one download to reach the internet. Sovereign mode stays on for everything else.",
                    )
                    .clicked()
                {
                    self.download(m, true);
                }
                if ui.button("Not now").clicked() {
                    self.sovereign_blocked = None;
                }
            });
        }
        if let Some((text, error)) = self.notes.get(&m.id) {
            if *error {
                ui.colored_label(Color32::from_rgb(213, 94, 0), text);
            } else {
                ui.weak(text);
            }
        }
    }

    fn download(&mut self, m: &Model, allow_internet_once: bool) {
        self.sovereign_blocked = None;
        self.notes.remove(&m.id);
        let job = models::start_download(
            self.store.clone(),
            m.clone(),
            DownloadOptions {
                allow_internet_once,
                ..Default::default()
            },
        );
        self.downloads.insert(m.id.clone(), job);
    }

    /// The acceptance and remove confirmations.
    fn dialogs(&mut self, ctx: &egui::Context) {
        let cat = models::catalogue();
        if let Some(m) = self.accepting.as_deref().and_then(|id| cat.get(id)) {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("lsr-model-accept")).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.heading(format!("Accept the licence of {}?", m.name));
                ui.horizontal(|ui| {
                    ui.label("Licence class:");
                    badge_ui(ui, m.licence.class);
                });
                ui.label(format!("Licence: {}", m.licence.id));
                ui.label(match m.licence.class {
                    LicenceClass::CommercialSafe => {
                        "A permissive licence: you may use this model for any purpose, commercial included, under its terms."
                    }
                    LicenceClass::Restricted => {
                        "A restricted licence: commercial use is allowed only under its conditions (for example a revenue cap or registration with the publisher). Make sure your use qualifies."
                    }
                    LicenceClass::NonCommercial => {
                        "A non-commercial licence: research and personal use only. Do not use it in paid work."
                    }
                });
                if ui.link("Read the licence").clicked() {
                    open_url(&m.licence.url);
                }
                ui.weak("Your acceptance is recorded on this machine, not in the project.");
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("I accept the {} licence", m.licence.class))
                        .clicked()
                    {
                        let (store, model) = (self.store.clone(), m.clone());
                        self.writes.insert(
                            m.id.clone(),
                            Job::spawn("lsr-models-accept", move |_| {
                                let path = store.accepted_path();
                                let mut a = Acceptances::load(path)?;
                                a.accept(&model, models::today());
                                a.save(path)?;
                                Ok("Licence accepted.".to_owned())
                            }),
                        );
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.accepting = None;
            }
        }
        if let Some(m) = self.removing.as_deref().and_then(|id| cat.get(id)) {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("lsr-model-remove")).show(ctx, |ui| {
                ui.heading(format!("Remove {}?", m.name));
                ui.label(format!(
                    "Deletes {} from this machine. Projects that use it will need it downloaded again.",
                    self.store.model_dir(&m.id).display()
                ));
                ui.horizontal(|ui| {
                    if ui.button("Remove").clicked() {
                        let dir = self.store.model_dir(&m.id);
                        self.writes.insert(
                            m.id.clone(),
                            Job::spawn("lsr-models-remove", move |_| {
                                std::fs::remove_dir_all(&dir)
                                    .map(|()| "Removed.".to_owned())
                                    .map_err(|e| format!("{}: {e}", dir.display()))
                            }),
                        );
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.removing = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_carry_their_meaning_in_text() {
        let texts: Vec<&str> = LicenceClass::ALL.iter().map(|c| badge(*c).1).collect();
        assert_eq!(texts, ["COMMERCIAL-SAFE", "RESTRICTED", "NON-COMMERCIAL"]);
        let colours: std::collections::HashSet<_> =
            LicenceClass::ALL.iter().map(|c| badge(*c).0).collect();
        assert_eq!(colours.len(), 3);
    }

    #[test]
    fn a_closed_window_starts_no_work() {
        let ctx = egui::Context::default();
        let mut panel = ModelsPanel::default();
        let mut ai = AiSettings::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            panel.window(ui.ctx(), &mut ai, &BTreeMap::new());
        });
        out.textures_delta.clear();
        assert!(panel.refresh.is_none() && panel.downloads.is_empty());
    }

    #[test]
    fn the_open_window_reads_the_cache_off_the_ui_thread_and_renders() {
        let dir = std::env::temp_dir().join(format!("lsr-app-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut panel = ModelsPanel {
            open: true,
            store: Store::new(dir.join("models")),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut ai = AiSettings {
            commercial_safe_only: true,
            ..AiSettings::default()
        };
        let manifest = BTreeMap::from([(
            "sd-turbo".to_owned(),
            models::pin(models::catalogue().get("sd-turbo").unwrap()),
        )]);
        let mut frame = |panel: &mut ModelsPanel| {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                panel.window(ui.ctx(), &mut ai, &manifest);
            });
            out.textures_delta.clear();
        };
        frame(&mut panel);
        assert!(panel.refresh.is_some(), "the first frame starts a read");
        let start = Instant::now();
        while panel.statuses.is_empty() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
            frame(&mut panel);
        }
        assert_eq!(panel.statuses.get("taesd"), Some(&Status::NotDownloaded));
        panel.accepting = Some("sd-turbo".into());
        panel.removing = Some("taesd".into());
        frame(&mut panel);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_text(2_584_000_000), "2.58 GB");
        assert_eq!(size_text(9_793_889), "9.8 MB");
    }
}
