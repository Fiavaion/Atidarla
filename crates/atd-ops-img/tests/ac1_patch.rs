//! M1 AC1 pattern without the project format (which waits on PRD A11): an
//! audio-reactive feedback patch built from edits, saved to JSON, loaded
//! into a second engine, and cooked in lock step. The reloaded network is
//! identical, both engines produce identical pixels, and the image follows
//! the audio.

use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

fn engine() -> Option<Engine> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 32,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

/// Two seconds of 1 kHz tone bursts: 0.25 s on, 0.25 s off.
fn bursts() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("atd-ac1-{}.wav", std::process::id()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..96_000 {
        let on = (i / 12_000) % 2 == 0;
        let s = if on {
            (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin() * 0.8
        } else {
            0.0
        };
        w.write_sample((s * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    path
}

/// Builds the patch; returns (audio, output) nodes.
fn build(e: &mut Engine, wav: &str) -> (NodeId, NodeId) {
    let mut ids = Vec::new();
    let mut edits = Vec::new();
    let mut node = |e: &mut Engine, ty: &str, name: &str, params: &[(&str, Value)]| {
        let id = e.alloc_id();
        ids.push(id);
        edits.push(Edit::AddNode {
            node: Node::new(id, ty, name),
        });
        for (p, v) in params {
            edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
        id
    };
    let audio = node(
        e,
        "sig.audio_file_in",
        "audio1",
        &[("file", json!(wav)), ("play", json!(false))],
    );
    let spectrum = node(e, "sig.spectrum", "spectrum1", &[("smoothing", json!(0.0))]);
    let noise = node(e, "img.noise", "noise1", &[("monochrome", json!(false))]);
    let fb = node(e, "img.feedback", "feedback1", &[]);
    let xform = node(
        e,
        "img.transform",
        "transform1",
        &[("scale", json!([0.98, 0.98]))],
    );
    let comp = node(
        e,
        "img.composite",
        "composite1",
        &[("operation", json!("over")), ("opacity", json!(0.15))],
    );
    let out = node(e, "img.level", "level1", &[]);
    let wire = |a: NodeId, b: NodeId, input: &str| Edit::Connect {
        wire: Wire::new(Endpoint::new(a, "out"), Endpoint::new(b, input)),
    };
    edits.extend([
        wire(audio, spectrum, "in"),
        wire(noise, fb, "in"),
        wire(fb, xform, "in"),
        wire(noise, comp, "a"),
        wire(xform, comp, "b"),
        wire(comp, out, "in"),
        // The loop: the output's previous frame feeds the feedback node.
        wire(out, fb, "loop"),
        Edit::SetParam {
            node: out,
            param: "brightness".into(),
            value: Some(Param::bound(json!(0.0), spectrum, "level")),
        },
        Edit::SetParam {
            node: xform,
            param: "rotate".into(),
            value: Some(Param::bound(json!(0.0), spectrum, "band5")),
        },
    ]);
    e.edit("build patch", edits).unwrap();
    (audio, out)
}

fn pixels(e: &Engine, node: NodeId) -> Vec<u8> {
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()
}

/// Cooks until the WAV has decoded (Audio File In names its channels),
/// then starts playback.
fn ready(e: &mut Engine, audio: NodeId, out: NodeId) {
    let start = std::time::Instant::now();
    while e.channels(audio).is_none_or(|c| c.is_empty()) {
        e.cook(&[out], 0.0).unwrap();
        assert!(start.elapsed().as_secs() < 10, "audio decoded");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    e.edit(
        "play",
        [Edit::SetParam {
            node: audio,
            param: "play".into(),
            value: Some(Param::constant(json!(true))),
        }],
    )
    .unwrap();
}

#[test]
fn audio_reactive_feedback_patch_round_trips_and_reacts() {
    let Some(mut a) = engine() else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let mut b = engine().unwrap();
    let wav = bursts();
    let (audio, out) = build(&mut a, &wav.to_string_lossy());

    // Save and reload: the network text round-trips exactly.
    let saved = serde_json::to_string_pretty(a.network()).unwrap();
    let loaded: Network = serde_json::from_str(&saved).unwrap();
    b.load(loaded).unwrap();
    assert_eq!(b.network(), a.network());
    assert_eq!(serde_json::to_string_pretty(b.network()).unwrap(), saved);

    ready(&mut a, audio, out);
    ready(&mut b, audio, out);
    // Two seconds at 60 fps, in lock step.
    let mut brightness_on = Vec::new();
    let mut brightness_off = Vec::new();
    for f in 1..=120 {
        let t = f64::from(f) / 60.0;
        a.cook(&[out], t).unwrap();
        b.cook(&[out], t).unwrap();
        let (pa, pb) = (pixels(&a, out), pixels(&b, out));
        assert_eq!(pa, pb, "frame {f}: the reloaded patch renders identically");
        // Mean of RGB (alpha stays 255).
        let rgb: Vec<f64> = pa
            .chunks_exact(4)
            .flat_map(|p| p[..3].iter().map(|v| f64::from(*v)))
            .collect();
        let mean = rgb.iter().sum::<f64>() / rgb.len() as f64;
        // Skip frames whose analysis window straddles an edge.
        let phase = (t % 0.5) / 0.5;
        if (0.15..0.45).contains(&phase) {
            brightness_on.push(mean);
        } else if (0.65..0.95).contains(&phase) {
            brightness_off.push(mean);
        }
    }
    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (on, off) = (avg(&brightness_on), avg(&brightness_off));
    assert!(
        off < 2.0 && on > 10.0,
        "the image follows the audio: {on:.1} with tone, {off:.1} in silence"
    );
    std::fs::remove_file(wav).ok();
}
