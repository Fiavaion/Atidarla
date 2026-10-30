//! Supervisor, link and router together (FR-A2, FR-A7, PT-9): a node's
//! frames come back processed, a killed host is restarted and frames resume
//! within 5 s, and a route picks a LAN host that meets the real-time need.

mod support;

use std::time::{Duration, Instant};

use atd_ai::link::{FrameParams, Link, LinkConfig, LinkState, ModelSpec};
use atd_ai::router::{Route, Target};
use atd_ai::supervisor::{HostKind, HostSpec, HostState, Supervisor};
use support::{fill, inverted, listen};

fn leak(s: Supervisor) -> &'static Supervisor {
    Box::leak(Box::new(s))
}

fn config(route: &str, vendor: Option<&str>) -> LinkConfig {
    LinkConfig {
        route: Route::parse(route).unwrap(),
        model: ModelSpec {
            model_id: "builtin.invert".into(),
            adapter: "builtin".into(),
            path: None,
            options: serde_json::Value::Null,
        },
        realtime_vendor: vendor.map(str::to_owned),
        in_flight: 2,
        budget_ms: 0.0,
        max_size: (64, 64),
    }
}

/// Offers frames at ~60 fps until `until` holds for a result, checking each
/// result is the inverse of the frame it answers. Returns how long it took.
/// Offering and taking never wait: each call is timed.
fn drive(
    link: &Link,
    sent: &mut Vec<Vec<u8>>,
    limit: Duration,
    mut until: impl FnMut(&Link) -> bool,
) -> Duration {
    let t0 = Instant::now();
    let params = FrameParams::default();
    let mut seen = link.result_seq();
    let mut worst = Duration::ZERO;
    while t0.elapsed() < limit {
        let seq = sent.len() as u64 + 1;
        let mut frame = vec![0u8; 32 * 16 * 4];
        fill(&mut frame, seq);
        let c = Instant::now();
        if link.offer(32, 16, seq * 16_667, &params, |px| {
            px.copy_from_slice(&frame)
        }) {
            sent.push(frame);
        }
        link.take(&mut seen, |out| {
            let input = &sent[out.seq as usize - 1];
            assert_eq!(out.pixels, inverted(input), "result for frame {}", out.seq);
        });
        worst = worst.max(c.elapsed());
        if until(link) {
            assert!(
                worst < Duration::from_millis(5),
                "the render side waited {worst:?}"
            );
            return t0.elapsed();
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    panic!("not done within {limit:?}: {:?}", link.status());
}

#[test]
fn frames_flow_and_a_killed_host_is_back_within_five_seconds() {
    let sup = leak(Supervisor::new());
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Local(support::ref_host()),
    });
    let link = Link::new(sup, config("local", None));
    let mut sent = Vec::new();
    drive(&link, &mut sent, Duration::from_secs(20), |l| l.status().frames >= 20);
    assert_eq!(link.status().state, LinkState::Running);

    let pid = sup.host("local").unwrap().pid.expect("local host pid");
    kill(pid);
    let frames = link.status().frames;
    let took = drive(&link, &mut sent, Duration::from_secs(10), |l| {
        l.status().frames >= frames + 10 && l.status().reconnects >= 1
    });
    println!("PT-9: frames again {took:?} after the host was killed");
    assert!(took < Duration::from_secs(5), "{took:?}");
    let host = sup.host("local").unwrap();
    assert_eq!(host.state, HostState::Ready);
    assert_eq!(host.restarts, 1);
    assert_ne!(host.pid, Some(pid));
}

#[test]
fn a_realtime_node_routes_to_the_nvidia_lan_host() {
    let pc = listen(
        &support::ref_host(),
        &["--token", "s3cret", "--fake-vendor", "nvidia"],
    );
    let sup = leak(Supervisor::new());
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Local(support::ref_host()),
    });
    sup.set(HostSpec {
        name: "lan:pc".into(),
        kind: HostKind::Remote {
            addr: pc.addr.clone(),
            token: Some("s3cret".into()),
        },
    });
    sup.want("lan:pc");
    let link = Link::new(sup, config("local → lan:pc", Some("nvidia")));
    let mut sent = Vec::new();
    drive(&link, &mut sent, Duration::from_secs(20), |l| l.status().frames >= 10);
    let s = link.status();
    assert_eq!(s.target, Some(Target::Lan("pc".into())));
    assert!(s.realtime);
    // The PC goes away: the node falls back to the Mac at interactive rates.
    drop(pc);
    let frames = link.status().frames;
    drive(&link, &mut sent, Duration::from_secs(20), |l| {
        let s = l.status();
        s.frames >= frames + 10 && s.target == Some(Target::Local)
    });
    assert!(!link.status().realtime);
}

fn kill(pid: u32) {
    let status = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
    } else {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
    };
    assert!(status.unwrap().success());
}
