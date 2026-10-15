//! Spike (d) benchmark: round-trip latency of RGBA8 frames through the
//! shared-memory ring and the Python reference host.
//!
//! `cargo run -p atd-ai --release --example spike_d [frames]`
//! Set `ATD_PYTHON` to choose the interpreter.

use std::path::PathBuf;
use std::time::Instant;

use atd_ai::protocol::FrameSubmit;
use atd_ai::{HostClient, HostCommand, RingLayout};

fn main() {
    let frames: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let python = std::env::var("ATD_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../ai-host/src")
        .canonicalize()
        .expect("ai-host/src");
    let cmd = HostCommand::python_module(python, src);

    println!(
        "| size | frames | spawn+open ms | p50 ms | p95 ms | max ms | host p50 ms | fps (serial) |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for side in [256u32, 512, 1024] {
        let t0 = Instant::now();
        let mut client = HostClient::spawn(
            &cmd,
            RingLayout {
                slot_count: 4,
                width: side,
                height: side,
            },
        )
        .expect("spawn");
        let spawn_ms = t0.elapsed().as_secs_f64() * 1e3;
        client.load_model("builtin.invert").expect("load");

        let mut rtt = Vec::with_capacity(frames as usize);
        let mut host = Vec::with_capacity(frames as usize);
        let start = Instant::now();
        for seq in 1..=frames {
            let input = (seq % 2) as u32;
            client.ring_mut().frame_mut(input)[0] = seq as u8;
            client.ring_mut().set_seq(input, seq);
            let submit = FrameSubmit {
                seq,
                timestamp_us: 0,
                model_id: "builtin.invert".into(),
                input_slot: input,
                output_slot: input + 2,
            };
            let t = Instant::now();
            let result = client.process(&submit).expect("frame");
            rtt.push(t.elapsed().as_secs_f64() * 1e3);
            host.push(result.host_ms);
        }
        let fps = frames as f64 / start.elapsed().as_secs_f64();
        rtt.sort_by(f64::total_cmp);
        host.sort_by(f64::total_cmp);
        let pct = |v: &[f64], p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        println!(
            "| {side}² | {frames} | {spawn_ms:.0} | {:.2} | {:.2} | {:.2} | {:.2} | {fps:.0} |",
            pct(&rtt, 0.5),
            pct(&rtt, 0.95),
            rtt[rtt.len() - 1],
            pct(&host, 0.5)
        );
        client.close().expect("close");
    }
}
