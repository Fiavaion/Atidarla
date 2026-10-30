//! The speech-to-text link (FR-A13) against the reference host in this
//! process: utterances are transcribed by `builtin.stt_echo`, offering never
//! waits, a busy mailbox keeps the utterance, and a route nobody serves
//! says why.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use atd_ai::link::LinkState;
use atd_ai::refhost::{RefHostOptions, serve};
use atd_ai::router::Route;
use atd_ai::speech::{SttConfig, SttLink};
use atd_ai::supervisor::{HostKind, HostSpec, Supervisor};
use serde_json::json;

fn supervisor() -> &'static Supervisor {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || serve(listener, RefHostOptions::default()));
    let sup: &'static Supervisor = Box::leak(Box::new(Supervisor::new()));
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Remote { addr, token: None },
    });
    sup
}

fn config(route: &str) -> SttConfig {
    SttConfig {
        route: Route::parse(route).unwrap(),
        model_id: "builtin.stt_echo".into(),
        adapter: "builtin".into(),
        path: None,
        sample_rate: 16_000,
        params: json!({"language": "en"}),
    }
}

fn wait_text(link: &SttLink, seen: &mut u64) -> (u64, String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut got = None;
        link.take(seen, |h| got = Some((h.seq, h.text.clone())));
        if let Some(g) = got {
            return g;
        }
        assert!(
            Instant::now() < deadline,
            "no transcript: {:?}",
            link.with_status(Clone::clone)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn utterances_are_transcribed_in_order_without_waiting() {
    let link = SttLink::new(supervisor(), config("local"), 16_000);
    assert_eq!(link.capacity(), 16_000);
    let mut seen = 0;
    let mut speech = vec![0.0f32; 4_000];
    speech[100] = -0.75;
    let t0 = Instant::now();
    let seq = link.offer(&speech).expect("free mailbox");
    assert!(t0.elapsed() < Duration::from_millis(50), "offer waited");
    let (got, text) = wait_text(&link, &mut seen);
    assert_eq!(got, seq);
    assert_eq!(text, "4000 samples at 16000 Hz, peak 0.750");
    // Longer than the mailbox: cut to its capacity.
    let long = vec![0.25f32; 20_000];
    let deadline = Instant::now() + Duration::from_secs(5);
    let seq2 = loop {
        if let Some(s) = link.offer(&long) {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    let (got, text) = wait_text(&link, &mut seen);
    assert_eq!(got, seq2);
    assert_eq!(text, "16000 samples at 16000 Hz, peak 0.250");
    assert!(link.with_status(|s| s.utterances == 2 && s.state == LinkState::Running));
}

#[test]
fn a_full_mailbox_keeps_the_waiting_utterance() {
    // Nobody serves `lan:nowhere`: the worker never takes the utterance.
    let link = SttLink::new(supervisor(), config("lan:nowhere"), 1_000);
    assert!(link.offer(&[0.5; 10]).is_some());
    assert!(
        link.offer(&[0.1; 10]).is_none(),
        "overwrote a waiting utterance"
    );
    assert!(link.busy());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !link.with_status(|s| matches!(s.state, LinkState::Waiting(_))) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let why = link.with_status(|s| match &s.state {
        LinkState::Waiting(w) => w.clone(),
        _ => String::new(),
    });
    assert!(why.contains("lan:nowhere"), "{why}");
}
