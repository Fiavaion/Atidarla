//! Supervisor, link and router together (FR-A2, FR-A7, PT-9): a node's
//! frames come back processed, a killed host is restarted and frames resume
//! within 5 s, and a route picks a LAN host that meets the real-time need.

mod support;

use std::time::{Duration, Instant};

use atd_ai::link::{FrameParams, Link, LinkConfig, LinkState, ModelSpec, NoResult};
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
        if link
            .offer(32, 16, seq * 16_667, &params, |px| {
                px.copy_from_slice(&frame)
            })
            .is_some()
        {
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
    drive(&link, &mut sent, Duration::from_secs(20), |l| {
        l.status().frames >= 20
    });
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
    drive(&link, &mut sent, Duration::from_secs(20), |l| {
        l.status().frames >= 10
    });
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

/// The fail-fast bound for an offline wait with no host: strict on quiet
/// hardware (`ATD_TIMING_STRICT=1`), slack for a shared CI runner.
fn fail_fast_bound() -> Duration {
    if std::env::var("ATD_TIMING_STRICT").is_ok_and(|v| v == "1") {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(2)
    }
}

/// Offers frame `seq` the way an offline render does.
fn offer_offline(link: &Link, seq: u64) -> (u64, Vec<u8>) {
    let mut frame = vec![0u8; 32 * 16 * 4];
    fill(&mut frame, seq);
    let n = link.offer_blocking(32, 16, seq * 33_333, &FrameParams::default(), |px| {
        px.copy_from_slice(&frame)
    });
    (n, frame)
}

/// Offline with no AI host at all, the wait ends at once and says so,
/// instead of waiting out the 30 s give-up per frame.
#[test]
fn offline_with_no_host_fails_fast() {
    let sup = leak(Supervisor::new());
    let link = Link::new(sup, config("local", None));
    for i in 1..=3 {
        let (seq, _) = offer_offline(&link, i);
        let t0 = Instant::now();
        let r = link.wait_result(seq, Duration::from_secs(120), |_| {});
        let took = t0.elapsed();
        println!("no host, frame {i}: {r:?} after {took:?}");
        match r {
            Err(NoResult::Unreachable(why)) => assert!(why.contains("not configured"), "{why}"),
            other => panic!("want Unreachable, got {other:?}"),
        }
        assert!(took < fail_fast_bound(), "took {took:?}");
    }
    assert!(link.status().unreachable);
}

/// A host that refuses to load the model is not final (it may still be
/// downloading it): the offline wait keeps trying instead of failing fast.
#[test]
fn a_refused_model_load_is_retried_not_final() {
    let sup = leak(Supervisor::new());
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Local(support::ref_host()),
    });
    let mut cfg = config("local", None);
    cfg.model.model_id = "builtin.no_such_model".into();
    let link = Link::new(sup, cfg);
    let (seq, _) = offer_offline(&link, 1);
    let t0 = Instant::now();
    let r = link.wait_result(seq, Duration::from_secs(3), |_| {});
    println!("refused model: {r:?} after {:?}", t0.elapsed());
    assert!(
        !matches!(r, Err(NoResult::Unreachable(_))),
        "a refused model load must not be final: {r:?}"
    );
    assert!(!link.status().unreachable);
}

/// A remote host that refuses the connection (nothing listens) is out of
/// reach too; a route with every target out fails fast.
#[test]
fn offline_with_only_refusing_hosts_fails_fast() {
    // A port nothing listens on: bind, note it, close.
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let sup = leak(Supervisor::new());
    sup.set(HostSpec {
        name: "lan:gone".into(),
        kind: HostKind::Remote { addr, token: None },
    });
    let link = Link::new(sup, config("lan:gone → lan:never", None));
    let (seq, _) = offer_offline(&link, 1);
    let t0 = Instant::now();
    let r = link.wait_result(seq, Duration::from_secs(120), |_| {});
    let took = t0.elapsed();
    println!("refusing host: {r:?} after {took:?}");
    assert!(matches!(r, Err(NoResult::Unreachable(_))), "{r:?}");
    assert!(took < fail_fast_bound(), "took {took:?}");
}

/// Offline, a host that is up but slow is waited for: every frame gets its
/// own result (determinism), not an error and not an older frame's result.
#[test]
fn offline_waits_for_a_slow_host() {
    let slow = listen(&support::ref_host(), &["--delay-ms", "400"]);
    let sup = leak(Supervisor::new());
    sup.set(HostSpec {
        name: "lan:slow".into(),
        kind: HostKind::Remote {
            addr: slow.addr.clone(),
            token: None,
        },
    });
    let mut cfg = config("lan:slow", None);
    cfg.in_flight = 1;
    let link = Link::new(sup, cfg);
    for i in 1..=3 {
        let (seq, frame) = offer_offline(&link, i);
        let t0 = Instant::now();
        let mut got = None;
        let r = link.wait_result(seq, Duration::from_secs(120), |out| {
            got = Some((out.seq, out.pixels.clone()));
        });
        let took = t0.elapsed();
        println!("slow host, frame {i}: {r:?} after {took:?}");
        assert_eq!(r, Ok(()));
        let (got_seq, pixels) = got.unwrap();
        assert_eq!(got_seq, seq);
        assert_eq!(pixels, inverted(&frame), "result for frame {i}");
        assert!(took >= Duration::from_millis(350), "did not wait: {took:?}");
    }
}

/// A host that dies while a frame is with it ends the wait with an error,
/// instead of hanging to the full timeout.
#[test]
fn a_host_that_dies_mid_wait_ends_the_offline_wait() {
    let mut cmd = support::ref_host();
    cmd.args = vec!["--delay-ms".into(), "20000".into()];
    let sup = leak(Supervisor::new());
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Local(cmd),
    });
    let mut cfg = config("local", None);
    cfg.in_flight = 1;
    let link = Link::new(sup, cfg);
    let t0 = Instant::now();
    while link.status().state != LinkState::Running {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "{:?}",
            link.status()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let (seq, _) = offer_offline(&link, 1);
    let pid = sup.host("local").unwrap().pid.expect("local host pid");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        kill(pid);
    });
    let t0 = Instant::now();
    let r = link.wait_result(seq, Duration::from_secs(120), |_| {});
    let took = t0.elapsed();
    println!("host killed mid-wait: {r:?} after {took:?}");
    assert!(matches!(r, Err(NoResult::Lost(_))), "{r:?}");
    assert!(took < Duration::from_secs(5), "took {took:?}");
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
