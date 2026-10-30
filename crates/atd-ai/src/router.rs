//! Compute router (FR-A7): picks where an AI node runs from its route, a
//! policy list such as `local → lan:studio → cloud:runpod`, and the hosts the
//! supervisor knows.
//!
//! A node may need an adapter (`diffusion.sd`, `onnx`, …) and may run in
//! real time only on some hardware (the `win+nvidia` capability flag of the
//! Diffusion node, A7/A19). The first host in route order that is ready, has
//! the adapter and meets the real-time need wins; when none meets it, the
//! first ready host with the adapter runs the node at interactive rates.
//! Cloud targets are skipped in Sovereign mode and once the project's spend
//! cap is reached. The node falls back to the next target when its host
//! fails or stays over its latency budget.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::egress;
use crate::supervisor::{HostSnapshot, HostState};

/// One place a node can run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Local,
    Lan(String),
    Cloud(String),
}

impl Target {
    /// The supervisor's name for the host.
    pub fn host_name(&self) -> String {
        match self {
            Self::Local => "local".into(),
            Self::Lan(n) => format!("lan:{n}"),
            Self::Cloud(n) => format!("cloud:{n}"),
        }
    }

    pub fn is_cloud(&self) -> bool {
        matches!(self, Self::Cloud(_))
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        match s.split_once(':') {
            None if s == "local" => Ok(Self::Local),
            Some(("lan", n)) if !n.trim().is_empty() => Ok(Self::Lan(n.trim().into())),
            Some(("cloud", n)) if !n.trim().is_empty() => Ok(Self::Cloud(n.trim().into())),
            _ => Err(format!(
                "unknown target {s:?}: use local, lan:<name> or cloud:<name>"
            )),
        }
    }
}

/// A node's ordered list of targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route(pub Vec<Target>);

impl Route {
    /// Parses `local → lan:studio → cloud:runpod` (also `->`, `,` or `>`).
    pub fn parse(s: &str) -> Result<Self, String> {
        let normalised = s.replace('→', ",").replace("->", ",").replace('>', ",");
        let targets = normalised
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .map(Target::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if targets.is_empty() {
            return Err("the route is empty".into());
        }
        Ok(Self(targets))
    }
}

/// What a node needs from a host.
#[derive(Debug, Clone, Copy)]
pub struct Need<'a> {
    pub adapter: &'a str,
    /// Vendor a host must have to run the node in real time (`nvidia` for
    /// the `win+nvidia` flag), or `None` when any host is real time.
    pub realtime_vendor: Option<&'a str>,
}

/// The router's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub target: Target,
    /// Whether the host meets the real-time need (else interactive rates).
    pub realtime: bool,
}

/// Why a target was passed over, for the node's status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Sovereign,
    SpendCap,
    NotReady(String),
    NoAdapter,
    Excluded,
}

/// Picks a target. `hosts` is the supervisor's snapshot; `exclude` holds
/// targets the node is falling back from. Returns the choice, or every
/// target with why it was skipped.
pub fn choose(
    route: &Route,
    need: Need<'_>,
    hosts: &[HostSnapshot],
    exclude: &[Target],
) -> Result<Choice, Vec<(Target, Skip)>> {
    let mut skipped = Vec::new();
    let mut usable: Vec<(Target, bool)> = Vec::new();
    for t in &route.0 {
        let skip = if exclude.contains(t) {
            Some(Skip::Excluded)
        } else if t.is_cloud() && egress::sovereign() {
            Some(Skip::Sovereign)
        } else if t.is_cloud() && spend().cap_reached() {
            Some(Skip::SpendCap)
        } else {
            let name = t.host_name();
            match hosts.iter().find(|h| h.name == name) {
                None => Some(Skip::NotReady("not configured".into())),
                Some(h) if h.state != HostState::Ready => {
                    Some(Skip::NotReady(h.state.label().into()))
                }
                Some(h) => match &h.caps {
                    Some(c) if c.adapters.iter().any(|a| a == need.adapter) => {
                        let rt = need.realtime_vendor.is_none_or(|v| c.vendor == v);
                        usable.push((t.clone(), rt));
                        None
                    }
                    _ => Some(Skip::NoAdapter),
                },
            }
        };
        if let Some(s) = skip {
            skipped.push((t.clone(), s));
        }
    }
    if let Some((t, _)) = usable.iter().find(|(_, rt)| *rt) {
        return Ok(Choice {
            target: t.clone(),
            realtime: true,
        });
    }
    match usable.into_iter().next() {
        Some((target, _)) => Ok(Choice {
            target,
            realtime: false,
        }),
        None => Err(skipped),
    }
}

/// Cloud spend for the open project (FR-A7): a running total and a cap, in
/// thousandths of a cent so per-frame charges add up exactly.
#[derive(Debug, Default)]
pub struct Spend {
    spent: AtomicU64,
    /// 0 = no cloud spending allowed.
    cap: AtomicU64,
    rates: Mutex<BTreeMap<String, f64>>,
}

const UNITS_PER_USD: f64 = 100_000.0;

impl Spend {
    pub fn set_cap_usd(&self, usd: f64) {
        self.cap
            .store((usd.max(0.0) * UNITS_PER_USD) as u64, Ordering::Release);
    }

    pub fn cap_usd(&self) -> f64 {
        self.cap.load(Ordering::Acquire) as f64 / UNITS_PER_USD
    }

    pub fn spent_usd(&self) -> f64 {
        self.spent.load(Ordering::Acquire) as f64 / UNITS_PER_USD
    }

    pub fn cap_reached(&self) -> bool {
        self.spent.load(Ordering::Acquire) >= self.cap.load(Ordering::Acquire)
    }

    /// Price of a cloud host per hour of use.
    pub fn set_rate(&self, host: &str, usd_per_hour: f64) {
        self.rates
            .lock()
            .expect("rates")
            .insert(host.to_owned(), usd_per_hour);
    }

    pub fn rate(&self, host: &str) -> Option<f64> {
        self.rates.lock().expect("rates").get(host).copied()
    }

    /// Charges `seconds` of use of `host`; returns the cost in USD.
    pub fn charge(&self, host: &str, seconds: f64) -> f64 {
        let usd = self.rate(host).unwrap_or(0.0) * seconds / 3600.0;
        self.spent
            .fetch_add((usd * UNITS_PER_USD).round() as u64, Ordering::AcqRel);
        usd
    }

    pub fn reset(&self) {
        self.spent.store(0, Ordering::Release);
    }
}

/// The process-wide meter (one project is open at a time).
pub fn spend() -> &'static Spend {
    static SPEND: std::sync::OnceLock<Spend> = std::sync::OnceLock::new();
    SPEND.get_or_init(Spend::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Capabilities;

    fn host(name: &str, vendor: &str, adapters: &[&str], state: HostState) -> HostSnapshot {
        HostSnapshot {
            name: name.into(),
            state,
            caps: Some(Capabilities {
                vendor: vendor.into(),
                adapters: adapters.iter().map(|s| (*s).into()).collect(),
                ..Capabilities::default()
            }),
            health: None,
            addr: Some("127.0.0.1:1".into()),
            restarts: 0,
            ready_at: None,
            pid: None,
        }
    }

    #[test]
    fn routes_parse() {
        let r = Route::parse("local → lan:studio -> cloud:runpod").unwrap();
        assert_eq!(
            r.0,
            [
                Target::Local,
                Target::Lan("studio".into()),
                Target::Cloud("runpod".into())
            ]
        );
        assert!(Route::parse("").is_err());
        assert!(Route::parse("local, mars:x").is_err());
    }

    #[test]
    fn a_realtime_host_later_in_the_route_wins() {
        let hosts = [
            host("local", "apple", &["diffusion.sd"], HostState::Ready),
            host("lan:pc", "nvidia", &["diffusion.sd"], HostState::Ready),
        ];
        let route = Route::parse("local → lan:pc").unwrap();
        let need = Need {
            adapter: "diffusion.sd",
            realtime_vendor: Some("nvidia"),
        };
        let c = choose(&route, need, &hosts, &[]).unwrap();
        assert_eq!(c.target, Target::Lan("pc".into()));
        assert!(c.realtime);
        // Without the PC, the Mac runs it at interactive rates.
        let c = choose(&route, need, &hosts[..1], &[]).unwrap();
        assert_eq!((c.target, c.realtime), (Target::Local, false));
        // Falling back from the PC.
        let c = choose(&route, need, &hosts, &[Target::Lan("pc".into())]).unwrap();
        assert_eq!(c.target, Target::Local);
    }

    #[test]
    fn missing_adapters_and_unready_hosts_are_skipped() {
        let hosts = [
            host("local", "apple", &["builtin"], HostState::Ready),
            host("lan:pc", "nvidia", &["onnx"], HostState::Starting),
        ];
        let route = Route::parse("local, lan:pc").unwrap();
        let need = Need {
            adapter: "onnx",
            realtime_vendor: None,
        };
        let why = choose(&route, need, &hosts, &[]).unwrap_err();
        assert_eq!(why[0], (Target::Local, Skip::NoAdapter));
        assert_eq!(
            why[1],
            (Target::Lan("pc".into()), Skip::NotReady("starting".into()))
        );
    }

    // The only test touching the process-wide Sovereign flag and meter.
    #[test]
    fn sovereign_mode_and_the_spend_cap_keep_nodes_off_the_cloud() {
        let _flags = egress::TEST_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
        let hosts = [host(
            "cloud:runpod",
            "nvidia",
            &["diffusion.sd"],
            HostState::Ready,
        )];
        let route = Route::parse("cloud:runpod").unwrap();
        let need = Need {
            adapter: "diffusion.sd",
            realtime_vendor: Some("nvidia"),
        };
        egress::set_sovereign(true);
        assert_eq!(
            choose(&route, need, &hosts, &[]).unwrap_err()[0].1,
            Skip::Sovereign
        );
        egress::set_sovereign(false);
        spend().reset();
        spend().set_cap_usd(1.0);
        spend().set_rate("cloud:runpod", 0.74);
        assert!(choose(&route, need, &hosts, &[]).is_ok());
        let cost = spend().charge("cloud:runpod", 3600.0);
        assert!((cost - 0.74).abs() < 1e-9);
        spend().charge("cloud:runpod", 3600.0);
        assert!(spend().cap_reached());
        assert_eq!(
            choose(&route, need, &hosts, &[]).unwrap_err()[0].1,
            Skip::SpendCap
        );
        spend().reset();
        spend().set_cap_usd(0.0);
        egress::set_sovereign(true);
    }
}
