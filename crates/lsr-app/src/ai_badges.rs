//! Badges on the node canvas (FR-A7, PRD A19): where each AI node runs
//! (from the engine's status line) and the capability flag an operator
//! falls back from on this machine (e.g. `win+nvidia` on a Mac).
//!
//! The status line is parsed in place (`&str` slices, no copies); the badge
//! text is written into a buffer the canvas reuses, so drawing allocates
//! nothing beyond what egui itself does.

use std::fmt::Write as _;

use egui::{
    Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use lsr_core::{Availability, CapabilityFlag, Fallback, OperatorSchema, PlatformInfo, Registry};

/// Where a host is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Local,
    Lan,
    Cloud,
    Unknown,
}

impl Scope {
    pub fn of(host: &str) -> Self {
        if host == "local" {
            Self::Local
        } else if host.starts_with("lan:") {
            Self::Lan
        } else if host.starts_with("cloud:") {
            Self::Cloud
        } else {
            Self::Unknown
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Local => "this machine",
            Self::Lan => "a LAN host",
            Self::Cloud => "a cloud host: frames leave the LAN and cost money",
            Self::Unknown => "an unknown host",
        }
    }
}

/// An AI node's status line, parsed (see `lsr_ops_ai::node`):
/// `"<host> · <fps> AI fps · <ms> ms[ · interactive][ · <vram> MB][ · $<cost>]"`,
/// `"routing…"`, `"<host>: loading the model…"` or `"waiting: <why>"`, each
/// optionally ending in ` · $<cost>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteStatus<'a> {
    Running {
        host: &'a str,
        fps: &'a str,
        ms: &'a str,
        interactive: bool,
        vram_mb: Option<&'a str>,
        cost: Option<&'a str>,
    },
    Routing {
        cost: Option<&'a str>,
    },
    Loading {
        host: &'a str,
        cost: Option<&'a str>,
    },
    Waiting {
        why: &'a str,
        cost: Option<&'a str>,
    },
    /// Not an AI status line.
    Other(&'a str),
}

pub fn parse(status: &str) -> RouteStatus<'_> {
    let (s, cost) = match status.rsplit_once(" · $") {
        Some((head, c)) if c.parse::<f64>().is_ok() => (head, Some(c)),
        _ => (status, None),
    };
    if s == "routing…" {
        return RouteStatus::Routing { cost };
    }
    if let Some(why) = s.strip_prefix("waiting: ") {
        return RouteStatus::Waiting { why, cost };
    }
    if let Some(host) = s.strip_suffix(": loading the model…") {
        return RouteStatus::Loading { host, cost };
    }
    let mut parts = s.split(" · ");
    let (Some(host), Some(fps), Some(ms)) = (parts.next(), parts.next(), parts.next()) else {
        return RouteStatus::Other(status);
    };
    let (Some(fps), Some(ms)) = (fps.strip_suffix(" AI fps"), ms.strip_suffix(" ms")) else {
        return RouteStatus::Other(status);
    };
    let mut interactive = false;
    let mut vram_mb = None;
    for p in parts {
        if p == "interactive" && vram_mb.is_none() && !interactive {
            interactive = true;
        } else if let Some(v) = p.strip_suffix(" MB")
            && vram_mb.is_none()
        {
            vram_mb = Some(v);
        } else {
            return RouteStatus::Other(status);
        }
    }
    RouteStatus::Running {
        host,
        fps,
        ms,
        interactive,
        vram_mb,
        cost,
    }
}

impl RouteStatus<'_> {
    fn waiting(&self) -> bool {
        matches!(self, Self::Waiting { .. })
    }

    /// The badge text: where and how on the first line, numbers on the
    /// second. Words carry the state, not colour alone. Only characters the
    /// default fonts have (no emoji: the app ships without emoji fonts).
    pub fn write(&self, out: &mut String) {
        out.clear();
        let cost = |out: &mut String, c: Option<&str>| {
            if let Some(c) = c {
                let _ = write!(out, " · ${c}");
            }
        };
        let _ = match *self {
            Self::Running {
                host,
                fps,
                ms,
                interactive,
                vram_mb,
                cost: c,
            } => {
                let mode = if interactive {
                    "interactive"
                } else {
                    "real time"
                };
                let _ = write!(out, "{host} · {mode}\n{fps} fps · {ms} ms");
                if let Some(v) = vram_mb {
                    let _ = write!(out, " · {v} MB");
                }
                cost(out, c);
                Ok(())
            }
            Self::Routing { cost: c } => {
                out.push_str("routing…");
                cost(out, c);
                Ok(())
            }
            Self::Loading { host, cost: c } => {
                let _ = write!(out, "{host}\nloading the model…");
                cost(out, c);
                Ok(())
            }
            Self::Waiting { cost: c, .. } => {
                out.push_str("! waiting");
                cost(out, c);
                Ok(())
            }
            Self::Other(s) => write!(out, "{s}"),
        };
    }

    /// The hover text (built only while hovered).
    pub fn explain(&self) -> String {
        let cost = |c: Option<&str>| c.map(|c| format!("\nEstimated cloud cost so far: ${c}"));
        match *self {
            Self::Running {
                host,
                fps,
                ms,
                interactive,
                vram_mb,
                cost: c,
            } => format!(
                "Runs on {host} ({}).\n{}\n{fps} AI frames per second, {ms} ms from submit to result.{}{}",
                Scope::of(host).describe(),
                if interactive {
                    "Interactive rates: no host on the route meets the node's real-time need."
                } else {
                    "Real time."
                },
                vram_mb
                    .map(|v| format!("\nHost GPU memory in use: {v} MB."))
                    .unwrap_or_default(),
                cost(c).unwrap_or_default(),
            ),
            Self::Routing { cost: c } => format!(
                "Choosing a host from the node's route.{}",
                cost(c).unwrap_or_default()
            ),
            Self::Loading { host, cost: c } => format!(
                "Opening a session on {host} ({}) and loading the model.{}",
                Scope::of(host).describe(),
                cost(c).unwrap_or_default()
            ),
            Self::Waiting { why, cost: c } => format!(
                "No host on the route can run this node now:\n{why}\n\nThe output holds the last good frame. \
                 See View → AI Hosts.{}",
                cost(c).unwrap_or_default()
            ),
            Self::Other(s) => s.to_owned(),
        }
    }
}

/// The capability flag an operator falls back from on `platform`, with its
/// fallback, or `None` when it runs natively (PRD A19). The node canvas
/// shows it as a badge, as the operator palette does.
pub fn capability_badge<'a>(
    schema: &'a OperatorSchema,
    platform: &PlatformInfo,
) -> Option<(&'a CapabilityFlag, &'a Fallback)> {
    match Registry::availability(schema, platform) {
        Availability::Native => None,
        Availability::Fallback { flag, fallback } => Some((flag, fallback)),
    }
}

/// Text size of badges at zoom 1.
const FONT: f32 = 10.0;

fn pill(painter: &Painter, rect: Rect, fill: Color32, stroke: Color32, zoom: f32) {
    let r = CornerRadius::same((4.0 * zoom).clamp(1.0, 8.0) as u8);
    painter.rect(rect, r, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
}

/// Draws an AI node's route badge centred under `node` (screen rect) from
/// its `status` line; `buf` is reused between nodes and frames. Hovering
/// it explains the route.
pub fn paint_route(
    ui: &Ui,
    painter: &Painter,
    node: Rect,
    id: u64,
    status: &str,
    zoom: f32,
    buf: &mut String,
) {
    let route = parse(status);
    route.write(buf);
    let warn = route.waiting();
    let text_colour = if warn {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().text_color()
    };
    let galley = painter.layout(
        buf.clone(),
        FontId::proportional(FONT * zoom),
        text_colour,
        f32::INFINITY,
    );
    let pad = Vec2::new(5.0, 2.0) * zoom;
    let size = galley.size() + pad * 2.0;
    let rect = Rect::from_min_size(
        Pos2::new(node.center().x - size.x / 2.0, node.max.y + 4.0 * zoom),
        size,
    );
    let fill = ui.visuals().extreme_bg_color.gamma_multiply(0.9);
    let stroke = if warn {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    pill(painter, rect, fill, stroke, zoom);
    painter.galley(rect.min + pad, galley, text_colour);
    let resp = ui.interact(rect, Id::unique(("lsr-ai-route", id)), Sense::hover());
    if resp.hovered() {
        resp.on_hover_text(route.explain());
    }
}

/// Draws the capability badge over the top-right corner of `node`.
pub fn paint_capability(
    ui: &Ui,
    painter: &Painter,
    node: Rect,
    id: u64,
    (flag, fallback): (&CapabilityFlag, &Fallback),
    zoom: f32,
    buf: &mut String,
) {
    buf.clear();
    let _ = write!(buf, "! {flag}");
    let colour = ui.visuals().warn_fg_color;
    let font = FontId::proportional(FONT * zoom);
    let galley = painter.layout_no_wrap(buf.clone(), font, colour);
    let pad = Vec2::new(4.0, 1.0) * zoom;
    let size = galley.size() + pad * 2.0;
    let rect = Rect::from_min_size(
        Pos2::new(node.max.x - size.x, node.min.y - size.y - 2.0 * zoom),
        size,
    );
    pill(painter, rect, ui.visuals().extreme_bg_color, colour, zoom);
    painter.galley(rect.min + pad, galley, colour);
    let resp = ui.interact(rect, Id::unique(("lsr-capability", id)), Sense::hover());
    if resp.hovered() {
        resp.on_hover_text(format!(
            "Needs {flag} for full quality; this machine does not meet it.\n{}",
            fallback.explanation()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsr_core::{Arch, GpuVendor, Os};

    #[test]
    fn every_status_form_parses() {
        assert_eq!(
            parse("lan:studio · 28.4 AI fps · 35 ms"),
            RouteStatus::Running {
                host: "lan:studio",
                fps: "28.4",
                ms: "35",
                interactive: false,
                vram_mb: None,
                cost: None
            }
        );
        assert_eq!(
            parse("local · 6.0 AI fps · 140 ms · interactive · 2048 MB"),
            RouteStatus::Running {
                host: "local",
                fps: "6.0",
                ms: "140",
                interactive: true,
                vram_mb: Some("2048"),
                cost: None
            }
        );
        assert_eq!(
            parse("cloud:runpod · 30.0 AI fps · 80 ms · 16000 MB · $0.12"),
            RouteStatus::Running {
                host: "cloud:runpod",
                fps: "30.0",
                ms: "80",
                interactive: false,
                vram_mb: Some("16000"),
                cost: Some("0.12")
            }
        );
        assert_eq!(parse("routing…"), RouteStatus::Routing { cost: None });
        assert_eq!(
            parse("local: loading the model…"),
            RouteStatus::Loading {
                host: "local",
                cost: None
            }
        );
        assert_eq!(
            parse("waiting: lan:studio not configured; cloud:x Sovereign mode · $1.50"),
            RouteStatus::Waiting {
                why: "lan:studio not configured; cloud:x Sovereign mode",
                cost: Some("1.50")
            }
        );
        // A reason that itself contains " · " stays whole.
        assert_eq!(
            parse("waiting: a · b"),
            RouteStatus::Waiting {
                why: "a · b",
                cost: None
            }
        );
        for other in ["Share Out: published", "x · y", "a · 1 AI fps · 2 ms · ?"] {
            assert_eq!(parse(other), RouteStatus::Other(other));
        }
    }

    #[test]
    fn badge_text_names_place_mode_and_numbers() {
        let mut buf = String::with_capacity(64);
        parse("local · 6.0 AI fps · 140 ms · interactive · 2048 MB").write(&mut buf);
        assert_eq!(buf, "local · interactive\n6.0 fps · 140 ms · 2048 MB");
        let cap = buf.capacity();
        parse("cloud:x · 30.0 AI fps · 80 ms · $0.12").write(&mut buf);
        assert_eq!(buf, "cloud:x · real time\n30.0 fps · 80 ms · $0.12");
        assert_eq!(buf.capacity(), cap, "the buffer is reused");
        parse("waiting: no host").write(&mut buf);
        assert_eq!(buf, "! waiting");
        assert!(parse("waiting: no host").explain().contains("no host"));
        assert!(
            parse("cloud:x · 1 AI fps · 2 ms")
                .explain()
                .contains("cost money")
        );
    }

    fn apple_mac() -> PlatformInfo {
        PlatformInfo {
            os: Os::MacOs,
            arch: Arch::Aarch64,
            gpu_vendor: GpuVendor::Apple,
            gpu_name: Some("Apple M-series".into()),
            gpu_backend: Some("Metal".into()),
            sdks: Default::default(),
        }
    }

    /// M4 acceptance: the Live AI example opened on a Mac shows the
    /// `win+nvidia` badge on its Diffusion node.
    #[test]
    fn the_live_ai_example_shows_win_nvidia_on_diffusion_on_a_mac() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/ai-live.lasair");
        let project = lsr_format::load(&dir).unwrap();
        let mut registry = Registry::default();
        lsr_ops_ai::register(&mut registry);
        let net = project.main().unwrap();
        let node = net
            .nodes()
            .find(|n| n.name == "diffusion1")
            .expect("diffusion1 in the example");
        let schema = registry.get(&node.type_id).unwrap();
        let (flag, fallback) = capability_badge(schema, &apple_mac()).expect("a badge on a Mac");
        assert_eq!(flag.to_string(), "win+nvidia");
        assert!(!fallback.explanation().is_empty());
        // A Windows/NVIDIA machine runs it natively: no badge.
        let pc = PlatformInfo {
            os: Os::Windows,
            arch: Arch::X86_64,
            gpu_vendor: GpuVendor::Nvidia,
            ..apple_mac()
        };
        assert!(capability_badge(schema, &pc).is_none());
        // Operators without flags never get one.
        let invert = registry.get("ai.model").unwrap();
        assert!(capability_badge(invert, &apple_mac()).is_none());
    }

    #[test]
    fn badge_text_uses_only_glyphs_the_default_fonts_have() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |_| {});
        out.textures_delta.clear();
        let mut buf = String::new();
        let mut texts = Vec::new();
        for s in [
            "cloud:x · 30.0 AI fps · 80 ms · interactive · 512 MB · $0.12",
            "routing…",
            "local: loading the model…",
            "waiting: no host · $1.00",
        ] {
            parse(s).write(&mut buf);
            texts.push(buf.clone());
        }
        texts.extend(["! win+nvidia".to_owned(), "$1.50".to_owned()]);
        ctx.fonts_mut(|f| {
            for t in &texts {
                assert!(
                    f.has_glyphs(&FontId::proportional(10.0), &t.replace('\n', "")),
                    "{t:?} has a glyph the default fonts lack"
                );
            }
        });
    }
}
