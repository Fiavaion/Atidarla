//! PT-8 (PRD §8, M3 AC4): MIDI/OSC input → visible change ≤ 2 frames.
//!
//! An OSC packet goes over loopback to an address mapped (as in
//! `mappings.toml`) to the red component of a Constant's colour. The loop
//! runs as the app does, paced at 60 fps: apply mappings
//! ([`MappingRuntime::update`] → `Engine::set_live`), cook, read the
//! output pixel back. We count frames from the send to the first frame
//! whose pixel shows the new value; frame 1 is the first frame cooked
//! after the send. The packet is sent a third of the way into a frame, so
//! frame 1 starts about 11 ms later. Latency is send → pixel read back.

use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use atd_core::graph::{Edit, Node};
use atd_format::Mappings;
use atd_format::mappings::{Source, Target};
use atd_gpu::GpuContext;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_io::mapping::{self, MappingRuntime};

/// One frame at 60 fps.
const FRAME: Duration = Duration::from_micros(16_667);

/// Frames allowed by PT-8.
pub const BUDGET_FRAMES: u32 = 2;

#[derive(Debug, Clone)]
pub struct Pt8 {
    pub trials: usize,
    /// Worst frames from send to visible change.
    pub max_frames: u32,
    /// Frames per trial.
    pub frames: Vec<u32>,
    pub median_ms: f64,
    pub max_ms: f64,
}

pub fn run(gpu: &GpuContext, trials: usize) -> anyhow::Result<Pt8> {
    let mut library = OpLibrary::default();
    atd_ops_img::library(&mut library);
    let mut engine = Engine::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        crate::builtin_registry(),
        library,
        EngineConfig {
            width: 16,
            height: 16,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    let node = engine.alloc_id();
    engine
        .edit(
            "add",
            [Edit::AddNode {
                node: Node::new(node, "img.constant", "constant1"),
            }],
        )
        .context("img.constant")?;

    let mut maps = Mappings::default();
    maps.add(
        Source::Osc {
            address: "/pt8/red".into(),
            arg: 0,
        },
        Target {
            node,
            param: "colour".into(),
            component: Some(0),
        },
        [0.0, 1.0],
    );
    let mut rt = MappingRuntime::new();
    let port = rt.listen_osc(0, true).context("OSC socket")?;
    let tx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;

    let frame = |engine: &mut Engine, rt: &mut MappingRuntime, t: f64| -> anyhow::Result<u8> {
        rt.update(
            &maps,
            |n, p| {
                mapping::resolve(
                    engine.network(),
                    engine.registry(),
                    n,
                    p,
                    engine.live_values().get(n, p),
                )
            },
            |n, p, i| engine.driven_component(n, p, i),
        );
        for (n, p, v) in rt.writes() {
            engine.set_live(n, p, Some(v));
        }
        engine.cook_frame(Some(node), t)?;
        let tex = engine.result(node).context("no output")?;
        let px = atd_interop::readback::read_rgba8(engine.device(), engine.queue(), tex)?;
        Ok(px[0])
    };

    // Frames start on a fixed 60 fps clock, as the app's do.
    let mut t = 0.0;
    let mut tick = Instant::now();
    let wait = |until: Instant| std::thread::sleep(until.saturating_duration_since(Instant::now()));
    for _ in 0..5 {
        wait(tick);
        tick += FRAME;
        frame(&mut engine, &mut rt, t)?;
        t += FRAME.as_secs_f64();
    }
    let mut frames = Vec::with_capacity(trials);
    let mut ms = Vec::with_capacity(trials);
    for i in 0..trials {
        let on = i % 2 == 0;
        let want: u8 = if on { 255 } else { 0 };
        let msg = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/pt8/red".into(),
            args: vec![rosc::OscType::Float(if on { 1.0 } else { 0.0 })],
        }))
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        // Send a third of the way into the frame that just ran, as a
        // controller would at any moment.
        wait(tick - FRAME + FRAME / 3);
        let sent = Instant::now();
        tx.send_to(&msg, (Ipv4Addr::LOCALHOST, port))?;
        let mut n = 0u32;
        loop {
            wait(tick);
            tick += FRAME;
            n += 1;
            let red = frame(&mut engine, &mut rt, t)?;
            t += FRAME.as_secs_f64();
            if red == want {
                ms.push(sent.elapsed().as_secs_f64() * 1e3);
                break;
            }
            anyhow::ensure!(n < 60, "no visible change within 60 frames");
        }
        frames.push(n);
    }
    ms.sort_by(f64::total_cmp);
    Ok(Pt8 {
        trials,
        max_frames: frames.iter().copied().max().unwrap_or(0),
        median_ms: ms[ms.len() / 2],
        max_ms: *ms.last().unwrap_or(&0.0),
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PT-8: OSC over loopback to a mapped parameter of a GPU node is
    /// visible within two frames.
    #[test]
    fn osc_to_pixel_within_two_frames() {
        let Ok(gpu) = GpuContext::headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let r = run(&gpu, 20).unwrap();
        eprintln!(
            "PT-8: frames {:?}, max {}, median {:.2} ms, max {:.2} ms",
            r.frames, r.max_frames, r.median_ms, r.max_ms
        );
        assert!(
            r.max_frames <= BUDGET_FRAMES,
            "PT-8: {} frames (budget {BUDGET_FRAMES})",
            r.max_frames
        );
    }
}
