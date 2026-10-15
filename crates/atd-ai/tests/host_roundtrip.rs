//! Spike (d): drives the real Python AI Host through the protocol prototype.

mod support;

use std::time::{Duration, Instant};

use atd_ai::protocol::{FrameSubmit, codes};
use atd_ai::{HostClient, PROTOCOL_VERSION, RingLayout};

const LAYOUT: RingLayout = RingLayout {
    slot_count: 4,
    width: 512,
    height: 512,
};

fn fill(frame: &mut [u8], seed: u64) {
    for (i, b) in frame.iter_mut().enumerate() {
        *b = (i as u64).wrapping_mul(31).wrapping_add(seed) as u8;
    }
}

fn inverted(frame: &[u8]) -> Vec<u8> {
    frame
        .chunks_exact(4)
        .flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], p[3]])
        .collect()
}

#[test]
fn session_capabilities_and_frames() {
    let mut client = HostClient::spawn(&support::host_command(), LAYOUT).expect("host starts");
    assert_eq!(client.session().protocol_version, PROTOCOL_VERSION);

    let caps = client.capabilities().unwrap();
    assert!(caps.transports.contains(&"shm".to_owned()));
    assert!(caps.models.contains(&"builtin.invert".to_owned()));
    client.load_model("builtin.invert").unwrap();

    for seq in 1..=16u64 {
        let input = (seq % 2) as u32;
        let output = 2 + (seq % 2) as u32;
        fill(client.ring_mut().frame_mut(input), seq);
        client.ring_mut().set_seq(input, seq);
        let submit = FrameSubmit {
            seq,
            timestamp_us: seq * 16_667,
            model_id: "builtin.invert".into(),
            input_slot: input,
            output_slot: output,
        };
        let result = client.process(&submit).unwrap();
        assert_eq!(result.output_slot, output);
        assert_eq!(
            client.ring().frame(output),
            inverted(client.ring().frame(input)).as_slice(),
            "frame {seq}"
        );
    }

    let health = client.health().unwrap();
    assert_eq!(health.frames_processed, 16);
    assert_eq!(health.loaded_models, ["builtin.invert"]);
    assert!(client.close().unwrap().success());
}

#[test]
fn host_errors_are_typed() {
    let mut client = HostClient::spawn(&support::host_command(), LAYOUT).unwrap();
    match client.load_model("no.such.model") {
        Err(atd_ai::client::ClientError::Rpc(e)) => assert_eq!(e.code, codes::UNKNOWN_MODEL),
        other => panic!("expected UNKNOWN_MODEL, got {other:?}"),
    }
    client.load_model("builtin.invert").unwrap();
    // Slot 0 was never stamped with frame 99.
    let submit = FrameSubmit {
        seq: 99,
        timestamp_us: 0,
        model_id: "builtin.invert".into(),
        input_slot: 0,
        output_slot: 1,
    };
    match client.process(&submit) {
        Err(atd_ai::client::ClientError::Rpc(e)) => assert_eq!(e.code, codes::BAD_SLOT),
        other => panic!("expected BAD_SLOT, got {other:?}"),
    }
    // The session survives errors.
    assert_eq!(client.health().unwrap().frames_processed, 0);
}

#[test]
fn killed_host_fails_fast_instead_of_hanging() {
    let mut client = HostClient::spawn(&support::host_command(), LAYOUT).unwrap();
    client.load_model("builtin.invert").unwrap();
    client.kill().unwrap();
    let start = Instant::now();
    assert!(client.health().is_err());
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "error within the call timeout"
    );
}
