//! Which hosts a wedge's items run on (FR-B2).
//!
//! A scheduler names AI Hosts (`local`, `lan:<x>`, `cloud:<x>`), every LAN
//! host (`lan:*`) or ComfyUI servers (`comfy:<host:port>`). AI Hosts are
//! asked to start (or connect) and given a few seconds to become ready;
//! those that are ready and have every adapter the item's AI nodes need
//! take items, the others are listed with the reason. Items are then
//! handed out from one queue to every host in turn as each finishes
//! (`run`), so equal hosts share the work evenly and a faster host takes
//! more. Cloud hosts are refused in Sovereign mode and once the spend cap
//! is reached, as for live AI nodes.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use atd_format::batch::{Scheduler, comfy_addr};

use crate::egress;
use crate::router::spend;
use crate::supervisor::{HostState, Supervisor};

/// The hosts that take items, and those passed over with why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picked {
    pub hosts: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// The hosts `scheduler` names that can run AI nodes needing `adapters`
/// (empty: the items have no AI nodes) and, for ComfyUI hosts, whether the
/// items have ComfyUI nodes (`comfy`). Waits up to `wait` for AI Hosts to
/// become ready. Blocks: call it from a worker or a CLI, never a render
/// thread.
pub fn pick_hosts(
    scheduler: &Scheduler,
    sup: &Supervisor,
    adapters: &BTreeSet<String>,
    comfy: bool,
    wait: Duration,
) -> Result<Picked, String> {
    // Without AI or ComfyUI nodes there is nothing to place: items cook
    // here whatever the scheduler says.
    if adapters.is_empty() && !comfy {
        return Ok(Picked {
            hosts: vec!["local".into()],
            skipped: Vec::new(),
        });
    }
    let names: Vec<String> = match scheduler {
        Scheduler::Lan => sup
            .snapshot()
            .into_iter()
            .map(|h| h.name)
            .filter(|n| n.starts_with("lan:"))
            .collect(),
        Scheduler::Hosts(h) => h.clone(),
    };
    if names.is_empty() {
        return Err(
            "the scheduler is lan:* but the project has no LAN hosts ([[ai.hosts]] in project.toml)"
                .into(),
        );
    }
    let mut picked = Picked::default();
    let mut ai = Vec::new();
    for n in names {
        if comfy_addr(&n).is_some() {
            if comfy {
                picked.hosts.push(n);
            } else {
                picked
                    .skipped
                    .push((n, "the target has no ComfyUI workflow nodes".into()));
            }
        } else if adapters.is_empty() {
            picked
                .skipped
                .push((n, "the target has no AI nodes".into()));
        } else if n.starts_with("cloud:") && egress::sovereign() {
            picked
                .skipped
                .push((n, "Sovereign mode keeps AI traffic off the internet".into()));
        } else if n.starts_with("cloud:") && spend().cap_reached() {
            picked
                .skipped
                .push((n, "the project's cloud spend cap is reached".into()));
        } else if sup.host(&n).is_none() {
            picked.skipped.push((n, "not configured".into()));
        } else {
            ai.push(n);
        }
    }
    for n in &ai {
        sup.want(n);
    }
    let start = Instant::now();
    loop {
        let settled = ai.iter().all(|n| {
            sup.host(n)
                .is_none_or(|h| matches!(h.state, HostState::Ready | HostState::Failed(_)))
        });
        if settled || start.elapsed() >= wait {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for n in ai {
        let Some(h) = sup.host(&n) else {
            picked.skipped.push((n, "removed".into()));
            continue;
        };
        match (&h.state, &h.caps) {
            (HostState::Ready, Some(caps)) => {
                let missing: Vec<&str> = adapters
                    .iter()
                    .filter(|a| !caps.adapters.contains(a))
                    .map(String::as_str)
                    .collect();
                if missing.is_empty() {
                    picked.hosts.push(n);
                } else {
                    picked
                        .skipped
                        .push((n, format!("cannot run {}", missing.join(", "))));
                }
            }
            (HostState::Failed(why) | HostState::Restarting(why), _) => {
                picked.skipped.push((n, why.clone()));
            }
            (state, _) => {
                picked
                    .skipped
                    .push((n, format!("still {} after {wait:?}", state.label())));
            }
        }
    }
    finish(picked)
}

fn finish(picked: Picked) -> Result<Picked, String> {
    if picked.hosts.is_empty() {
        let why: Vec<String> = picked
            .skipped
            .iter()
            .map(|(n, w)| format!("{n}: {w}"))
            .collect();
        return Err(format!("no host can run the items ({})", why.join("; ")));
    }
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refhost::{RefHostOptions, serve};
    use crate::supervisor::{HostKind, HostSpec};
    use std::net::TcpListener;

    fn host(sup: &Supervisor, name: &str, adapters_ok: bool) {
        let addr = if adapters_ok {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap().to_string();
            std::thread::spawn(move || serve(l, RefHostOptions::default()));
            a
        } else {
            // Nothing listens: the host fails.
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap().to_string();
            drop(l);
            a
        };
        sup.set(HostSpec {
            name: name.into(),
            kind: HostKind::Remote { addr, token: None },
        });
    }

    #[test]
    fn lan_star_takes_the_ready_hosts_with_the_adapter() {
        let sup = Supervisor::new();
        host(&sup, "lan:a", true);
        host(&sup, "lan:b", true);
        host(&sup, "lan:dead", false);
        let need = BTreeSet::from(["diffusion.sd".to_owned()]);
        let p = pick_hosts(&Scheduler::Lan, &sup, &need, false, Duration::from_secs(10)).unwrap();
        assert_eq!(p.hosts, ["lan:a", "lan:b"]);
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.skipped[0].0, "lan:dead");
        // The reference host has no `onnx` adapter.
        let need = BTreeSet::from(["onnx".to_owned()]);
        let err =
            pick_hosts(&Scheduler::Lan, &sup, &need, false, Duration::from_secs(10)).unwrap_err();
        assert!(err.contains("lan:a: cannot run onnx"), "{err}");
    }

    #[test]
    fn listed_hosts_keep_their_order_and_unknown_ones_are_named() {
        let sup = Supervisor::new();
        host(&sup, "lan:x", true);
        let s = Scheduler::Hosts(vec![
            "lan:nowhere".into(),
            "comfy:127.0.0.1:8188".into(),
            "lan:x".into(),
        ]);
        let need = BTreeSet::from(["builtin".to_owned()]);
        let p = pick_hosts(&s, &sup, &need, true, Duration::from_secs(10)).unwrap();
        assert_eq!(p.hosts, ["comfy:127.0.0.1:8188", "lan:x"]);
        assert_eq!(p.skipped, [("lan:nowhere".into(), "not configured".into())]);
        let none = Supervisor::new();
        let err = pick_hosts(&Scheduler::Lan, &none, &need, false, Duration::ZERO).unwrap_err();
        assert!(err.contains("no LAN hosts"), "{err}");
    }
}
