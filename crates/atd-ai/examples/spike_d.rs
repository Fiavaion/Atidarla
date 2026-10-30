//! Spike (d) benchmark, protocol v1: round-trip latency of RGBA8 frames
//! through the shared-memory ring to a local host started by the supervisor.
//!
//! `cargo run -p atd-ai --release --example spike_d [frames] [size]`
//! `ATD_AI_HOST_CMD` chooses the host (default: the Python host in `ai-host/`).

use std::time::{Duration, Instant};

use atd_ai::protocol::FrameSubmit;
use atd_ai::supervisor::{HostState, Supervisor};
use atd_ai::{FramePath, RingLayout};

fn main() {
    let mut args = std::env::args().skip(1);
    let frames: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(300);
    let size: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(512);
    let sup = Supervisor::global();
    sup.want("local");
    let t0 = Instant::now();
    while sup
        .host("local")
        .is_none_or(|h| h.state != HostState::Ready)
    {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "{:?}",
            sup.host("local")
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let start_ms = t0.elapsed().as_secs_f64() * 1e3;
    let layout = RingLayout {
        slot_count: 2,
        width: size,
        height: size,
    };
    let mut client = sup
        .session("local", FramePath::Shm(layout))
        .expect("session");
    client.load_builtin("builtin.invert").expect("model");
    let mut times = Vec::with_capacity(frames as usize);
    let mut host = Vec::with_capacity(frames as usize);
    for seq in 1..=frames {
        let ring = client.ring_mut().expect("shm");
        ring.frame_mut(0).fill(seq as u8);
        ring.set_seq(0, seq);
        let submit = FrameSubmit {
            seq,
            timestamp_us: seq * 16_667,
            model_id: "builtin.invert".into(),
            width: size,
            height: size,
            input_slot: Some(0),
            output_slot: Some(1),
            aux: Default::default(),
            params: serde_json::Value::Null,
        };
        let t = Instant::now();
        let reply = client
            .submit(&submit, None)
            .and_then(|p| p.wait())
            .expect("frame");
        let (r, _) = client.check_result(seq, reply).expect("result");
        times.push(t.elapsed().as_secs_f64() * 1e3);
        host.push(r.host_ms);
    }
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * p) as usize]
    };
    let (p50, p95, max) = (
        pct(&mut times, 0.5),
        pct(&mut times, 0.95),
        pct(&mut times, 1.0),
    );
    println!(
        "| {size}² | {frames} | {start_ms:.0} | {p50:.2} | {p95:.2} | {max:.2} | {:.2} | {:.0} |",
        pct(&mut host, 0.5),
        1e3 / p50
    );
    sup.shutdown();
}
