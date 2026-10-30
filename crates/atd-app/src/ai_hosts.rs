//! The project's AI settings in the app: Sovereign mode (FR-A8), the spend
//! cap (FR-A7) and LAN/cloud hosts, applied to the process when a project
//! opens or they change; the Sovereign badge in the top bar; and the AI
//! Hosts window (FR-A2): the supervisor's hosts, adding and removing remote
//! ones, and the spend meter.
//!
//! Applying only registers hosts (the supervisor connects when a node wants
//! one) and the window reads the supervisor's snapshot, a short lock, at
//! most twice a second: nothing here waits on a host (rule 1). Host tokens
//! go to the per-user `hosts.toml` (`atd_ai::hosts`), never the project.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use atd_ai::hosts::{self, HostTokens};
use atd_ai::router;
use atd_ai::supervisor::{HostSnapshot, HostState, Supervisor};
use atd_format::AiSettings;
use atd_format::models::{valid_host_addr, valid_host_name};
use egui::{Color32, RichText, Ui};

/// How often the window re-reads the supervisor.
const REFRESH: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub struct AiHostsPanel {
    pub open: bool,
    /// The settings last applied, and the project they belong to.
    applied: Option<(u64, AiSettings)>,
    tokens: HostTokens,
    tokens_path: Option<PathBuf>,
    hosts: Vec<HostSnapshot>,
    refreshed: Option<Instant>,
    /// Asking before Sovereign mode is turned off.
    confirm_off: bool,
    /// The add-host form.
    cloud: bool,
    name: String,
    addr: String,
    token: String,
    /// The last result to show: (text, is an error).
    note: Option<(String, bool)>,
}

impl AiHostsPanel {
    fn tokens_path(&mut self) -> PathBuf {
        self.tokens_path
            .get_or_insert_with(|| atd_ai::models::Store::default().hosts_path())
            .clone()
    }

    /// Applies the project's AI settings when a project opens (`opened`
    /// changes; the spend meter restarts) or they change. Compares in
    /// place, so an unchanged frame does no work.
    pub fn frame(&mut self, ai: &AiSettings, opened: u64) {
        let opening = self.applied.as_ref().is_none_or(|(o, _)| *o != opened);
        if !opening && self.applied.as_ref().is_some_and(|(_, a)| a == ai) {
            return;
        }
        if self.applied.is_none() {
            let path = self.tokens_path();
            self.tokens = HostTokens::load(&path).unwrap_or_else(|e| {
                self.note = Some((format!("AI host tokens: {e}"), true));
                HostTokens::default()
            });
        }
        hosts::apply(ai, &self.tokens, Supervisor::global());
        if opening {
            router::spend().reset();
        }
        self.applied = Some((opened, ai.clone()));
        self.refreshed = None;
    }

    /// The Sovereign badge for the top bar: "SOVEREIGN" when on, "CLOUD
    /// ALLOWED" when off, so the state never rests on colour alone (blue
    /// and orange also differ for common colour-vision deficiencies). Clicking turns it on at once, or asks before off.
    pub fn badge(&mut self, ui: &mut Ui, ai: &mut AiSettings) {
        let (text, fill, hover) = if ai.sovereign {
            (
                "SOVEREIGN",
                Color32::from_rgb(40, 90, 170),
                "Sovereign mode is on: AI traffic stays on this machine and the LAN; \
                 cloud hosts are never contacted.\nClick to allow cloud hosts.",
            )
        } else {
            (
                "CLOUD ALLOWED",
                Color32::from_rgb(170, 100, 20),
                "Sovereign mode is off: AI nodes may send frames to cloud hosts on their \
                 routes, within the spend cap.\nClick to turn Sovereign mode on.",
            )
        };
        let button = egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(fill)
            .corner_radius(8.0);
        if ui.add(button).on_hover_text(hover).clicked() {
            if ai.sovereign {
                self.confirm_off = true;
            } else {
                ai.sovereign = true;
            }
        }
    }

    /// The confirmation before Sovereign mode is turned off.
    pub fn confirm(&mut self, ctx: &egui::Context, ai: &mut AiSettings) {
        if !self.confirm_off {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("atd_sovereign_off")).show(ctx, |ui| {
            ui.heading("Turn Sovereign mode off?");
            ui.label(format!(
                "AI nodes may send frames to cloud providers on their routes; spending is \
                 capped at ${:.2}.",
                ai.spend_cap_usd
            ));
            if ai.spend_cap_usd == 0.0 {
                ui.weak("With a cap of $0 no cloud host runs: raise it in View → AI Hosts.");
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Allow cloud hosts").clicked() {
                    ai.sovereign = false;
                    self.confirm_off = false;
                }
                if ui.button("Keep Sovereign mode").clicked() {
                    self.confirm_off = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm_off = false;
        }
    }

    /// The AI Hosts window.
    pub fn window(&mut self, ctx: &egui::Context, ai: &mut AiSettings) {
        if !self.open {
            return;
        }
        if self.refreshed.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.hosts = Supervisor::global().snapshot();
            self.refreshed = Some(Instant::now());
        }
        ctx.request_repaint_after(REFRESH);
        let mut open = self.open;
        egui::Window::new("AI Hosts")
            .open(&mut open)
            .default_width(720.0)
            .show(ctx, |ui| self.contents(ui, ai));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut Ui, ai: &mut AiSettings) {
        ui.horizontal(|ui| {
            self.badge(ui, ai);
            ui.weak("Hosts are started or connected when an AI node first needs them.");
        });
        if ai.sovereign && ai.hosts.keys().any(|n| n.starts_with("cloud:")) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "! Sovereign mode blocks this project's cloud hosts: AI nodes skip them.",
            );
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| self.table(ui, ai));
        ui.separator();
        self.add_form(ui, ai);
        ui.separator();
        spend_meter(ui, ai);
        if let Some((text, error)) = &self.note {
            let colour = if *error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.colored_label(colour, text);
        }
    }

    fn table(&mut self, ui: &mut Ui, ai: &mut AiSettings) {
        let mut remove = None;
        egui::Grid::new("atd_ai_hosts")
            .num_columns(8)
            .striped(true)
            .show(ui, |ui| {
                for h in [
                    "Host", "State", "Hardware", "Adapters", "Restarts", "Running", "Address", "",
                ] {
                    ui.strong(h);
                }
                ui.end_row();
                for h in &self.hosts {
                    ui.monospace(&h.name);
                    state_label(ui, &h.state);
                    match &h.caps {
                        Some(c) => {
                            let devices: Vec<&str> =
                                c.devices.iter().map(|d| d.name.as_str()).collect();
                            let vram = c.vram_mb.map(|v| format!(" · {v} MB")).unwrap_or_default();
                            ui.label(format!("{}{vram}", c.vendor))
                                .on_hover_text(devices.join("\n"));
                            ui.label(c.adapters.join(", "));
                        }
                        None => {
                            ui.weak("—");
                            ui.weak("—");
                        }
                    }
                    ui.label(h.restarts.to_string());
                    match &h.health {
                        Some(health) => ui.label(format!(
                            "{} · {} frames",
                            uptime(health.uptime_s),
                            health.frames_processed
                        )),
                        None => ui.weak("—"),
                    };
                    let addr = h
                        .addr
                        .as_deref()
                        .or_else(|| ai.hosts.get(&h.name).map(String::as_str));
                    ui.monospace(addr.unwrap_or("—"));
                    if ai.hosts.contains_key(&h.name) {
                        if ui
                            .small_button("Remove")
                            .on_hover_text(
                                "Remove from this project (the token stays on this machine)",
                            )
                            .clicked()
                        {
                            remove = Some(h.name.clone());
                        }
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        if let Some(name) = remove {
            ai.hosts.remove(&name);
            self.note = Some((format!("Removed {name}"), false));
        }
    }

    fn add_form(&mut self, ui: &mut Ui, ai: &mut AiSettings) {
        ui.strong("Add a host");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.cloud, false, "LAN");
            ui.selectable_value(&mut self.cloud, true, "Cloud");
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .desired_width(100.0)
                    .hint_text("studio"),
            );
            ui.label("Address");
            ui.add(
                egui::TextEdit::singleline(&mut self.addr)
                    .desired_width(160.0)
                    .hint_text("192.168.1.20:7420"),
            );
            ui.label("Token");
            ui.add(
                egui::TextEdit::singleline(&mut self.token)
                    .password(true)
                    .desired_width(120.0),
            )
            .on_hover_text("Stored on this machine only, never in the project");
            if ui.button("Add").clicked() {
                self.note = Some(match self.add(ai) {
                    Ok(name) => (format!("Added {name}"), false),
                    Err(e) => (e, true),
                });
            }
        });
    }

    /// Adds the form's host to the project and its token to the user store.
    fn add(&mut self, ai: &mut AiSettings) -> Result<String, String> {
        let prefix = if self.cloud { "cloud" } else { "lan" };
        let name = format!("{prefix}:{}", self.name.trim());
        let addr = self.addr.trim().to_owned();
        if !valid_host_name(&name) {
            return Err(format!(
                "{name}: use a-z, 0-9, `-`, `_` and `.` for the name"
            ));
        }
        if !valid_host_addr(&addr) {
            return Err(format!("{addr:?}: the address must be host:port"));
        }
        if self.tokens.get(&name) != Some(self.token.as_str()) {
            self.tokens.set(&name, &self.token);
            let path = self.tokens_path();
            self.tokens.save(&path)?;
            // The token changed; apply again even if the project did not.
            if let Some((_, applied)) = &mut self.applied {
                applied.hosts.remove(&name);
            }
        }
        ai.hosts.insert(name.clone(), addr);
        self.name.clear();
        self.addr.clear();
        self.token.clear();
        Ok(name)
    }
}

fn state_label(ui: &mut Ui, state: &HostState) {
    let why = match state {
        HostState::Restarting(why) | HostState::Failed(why) => Some(why.as_str()),
        _ => None,
    };
    let (symbol, colour) = match state {
        HostState::Ready => ("", ui.visuals().text_color()),
        HostState::Idle => ("", ui.visuals().weak_text_color()),
        HostState::Starting => ("", ui.visuals().weak_text_color()),
        HostState::Restarting(_) | HostState::Failed(_) => ("! ", ui.visuals().warn_fg_color),
    };
    let r = ui.colored_label(colour, format!("{symbol}{}", state.label()));
    if let Some(why) = why {
        r.on_hover_text(why);
    }
}

fn uptime(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

fn spend_meter(ui: &mut Ui, ai: &mut AiSettings) {
    let spend = router::spend();
    ui.horizontal(|ui| {
        ui.strong("Cloud spend");
        ui.label(format!("${:.2} of", spend.spent_usd()));
        ui.add(
            egui::DragValue::new(&mut ai.spend_cap_usd)
                .prefix("$")
                .speed(0.5)
                .range(0.0..=100_000.0)
                .fixed_decimals(2),
        )
        .on_hover_text("The project's cap for this session; $0 allows no cloud spending");
        if spend.cap_reached() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "! cap reached: cloud hosts are skipped",
            );
        }
        if ui
            .button("Reset")
            .on_hover_text("Start counting from $0 again")
            .clicked()
        {
            spend.reset();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptimes_read_naturally() {
        assert_eq!(uptime(5.4), "5 s");
        assert_eq!(uptime(125.0), "2 min");
        assert_eq!(uptime(3_725.0), "1 h 2 min");
    }

    #[test]
    fn the_add_form_checks_names_and_addresses() {
        let mut panel = AiHostsPanel {
            name: "Studio".into(),
            addr: "10.0.0.2:7420".into(),
            ..Default::default()
        };
        let mut ai = AiSettings::default();
        assert!(panel.add(&mut ai).unwrap_err().contains("lan:Studio"));
        panel.name = "studio".into();
        panel.addr = "10.0.0.2".into();
        assert!(panel.add(&mut ai).unwrap_err().contains("host:port"));
        assert!(ai.hosts.is_empty());
    }

    /// The token goes to the user store; the saved project has none.
    #[test]
    fn tokens_never_reach_the_project() {
        let dir = std::env::temp_dir().join(format!("atd-app-hosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut panel = AiHostsPanel {
            cloud: true,
            name: "runpod".into(),
            addr: "pod.example.net:7420".into(),
            token: "tok-0123456789abcdef".into(),
            tokens_path: Some(dir.join("hosts.toml")),
            ..Default::default()
        };
        let mut project = atd_format::Project::new("p");
        assert_eq!(panel.add(&mut project.ai).unwrap(), "cloud:runpod");
        assert_eq!(project.ai.hosts["cloud:runpod"], "pod.example.net:7420");
        assert!(panel.token.is_empty(), "the form forgets the token");
        let stored = std::fs::read_to_string(dir.join("hosts.toml")).unwrap();
        assert!(stored.contains("tok-0123456789abcdef"), "{stored}");
        let text = atd_format::project_to_string(&project);
        assert!(text.contains("cloud:runpod"), "{text}");
        assert!(!text.contains("tok-0123456789abcdef"), "{text}");
        assert!(!text.contains("token"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
