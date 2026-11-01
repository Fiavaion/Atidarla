//! AI Host Protocol v1 end to end: sessions, both frame transports,
//! pipelining, typed errors and a killed host, against the Rust reference
//! host and the Python host.

mod support;

use std::time::{Duration, Instant};

use lsr_ai::protocol::{FrameSubmit, codes};
use lsr_ai::{ClientError, FramePath, HostClient, HostCommand, PROTOCOL_VERSION, RingLayout};
use support::{fill, inverted, listen};

const LAYOUT: RingLayout = RingLayout {
    slot_count: 4,
    width: 64,
    height: 48,
};

fn submit(seq: u64, w: u32, h: u32) -> FrameSubmit {
    FrameSubmit {
        seq,
        timestamp_us: seq * 16_667,
        model_id: "builtin.invert".into(),
        width: w,
        height: h,
        input_slot: None,
        output_slot: None,
        aux: Default::default(),
        params: serde_json::Value::Null,
    }
}

fn frames_through_both_transports(cmd: &HostCommand) {
    let host = listen(cmd, &["--token", "t0k"]);
    // Shared memory, frames smaller than the ring.
    let mut client = HostClient::connect(&host.addr, Some("t0k"), FramePath::Shm(LAYOUT)).unwrap();
    assert_eq!(client.session().protocol_version, PROTOCOL_VERSION);
    let caps = client.capabilities().unwrap();
    assert!(caps.transports.iter().any(|t| t == "shm"));
    assert!(caps.adapters.iter().any(|a| a == "builtin"));
    client.load_builtin("builtin.invert").unwrap();
    for seq in 1..=8u64 {
        let (w, h) = (32 + seq as u32, 16);
        let len = (w * h * 4) as usize;
        let k = (seq % 2) as u32;
        let ring = client.ring_mut().unwrap();
        fill(&mut ring.frame_mut(2 * k)[..len], seq);
        ring.set_seq(2 * k, seq);
        let mut s = submit(seq, w, h);
        s.input_slot = Some(2 * k);
        s.output_slot = Some(2 * k + 1);
        let reply = client.submit(&s, None).unwrap().wait().unwrap();
        let (r, _) = client.check_result(seq, reply).unwrap();
        assert_eq!((r.width, r.height), (w, h));
        let ring = client.ring().unwrap();
        assert_eq!(
            &ring.frame(2 * k + 1)[..len],
            inverted(&ring.frame(2 * k)[..len]).as_slice(),
            "frame {seq}"
        );
    }
    // Frames on the socket.
    let inline = HostClient::connect(&host.addr, Some("t0k"), FramePath::Inline).unwrap();
    inline.load_builtin("builtin.invert").unwrap();
    let mut px = vec![0; 20 * 10 * 4];
    fill(&mut px, 5);
    let reply = inline
        .submit(&submit(1, 20, 10), Some(&px))
        .unwrap()
        .wait()
        .unwrap();
    let (_, out) = inline.check_result(1, reply).unwrap();
    assert_eq!(out.unwrap(), inverted(&px));
    // Typed errors.
    let err = HostClient::connect(&host.addr, Some("wrong"), FramePath::Inline).unwrap_err();
    assert_eq!(err.code(), Some(codes::UNAUTHORIZED), "{err}");
    let err = inline.load_builtin("no.such").unwrap_err();
    assert_eq!(err.code(), Some(codes::UNKNOWN_MODEL), "{err}");
}

#[test]
fn reference_host_frames_through_both_transports() {
    frames_through_both_transports(&support::ref_host());
}

#[test]
fn python_host_frames_through_both_transports() {
    frames_through_both_transports(&support::python_host());
}

/// Health is answered while frames are in flight; several frames pipeline.
#[test]
fn requests_pipeline() {
    let host = listen(&support::ref_host(), &["--delay-ms", "60"]);
    let client = HostClient::connect(&host.addr, None, FramePath::Inline).unwrap();
    client.load_builtin("builtin.echo").unwrap();
    let px = vec![7u8; 8 * 8 * 4];
    let mut s = submit(1, 8, 8);
    s.model_id = "builtin.echo".into();
    let t0 = Instant::now();
    let a = client.submit(&s, Some(&px)).unwrap();
    s.seq = 2;
    let b = client.submit(&s, Some(&px)).unwrap();
    let health = client.health();
    let health_ms = t0.elapsed().as_millis();
    assert!(health.is_ok());
    assert!(
        health_ms < 60,
        "health waited for the frames: {health_ms} ms"
    );
    client.check_result(1, a.wait().unwrap()).unwrap();
    client.check_result(2, b.wait().unwrap()).unwrap();
}

/// A host that dies surfaces as an error at once, never a hang.
#[test]
fn a_killed_host_fails_fast() {
    let mut host = listen(&support::ref_host(), &["--delay-ms", "2000"]);
    let client = HostClient::connect(&host.addr, None, FramePath::Inline).unwrap();
    client.load_builtin("builtin.echo").unwrap();
    let mut s = submit(1, 4, 4);
    s.model_id = "builtin.echo".into();
    let pending = client.submit(&s, Some(&[0; 64])).unwrap();
    let t0 = Instant::now();
    host.child.kill().unwrap();
    let err = pending.wait().unwrap_err();
    assert!(matches!(err, ClientError::Disconnected), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(!client.alive());
}
