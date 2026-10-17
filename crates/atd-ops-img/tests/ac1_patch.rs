//! M1 AC1: the audio-reactive feedback example project
//! (`examples/audio-reactive-feedback.atidarla`) loaded from disk, saved to a
//! new folder (byte-identical files) and loaded from there into a second
//! engine. Both are cooked in lock step: identical pixels every frame, and
//! the image follows the audio. CI runs this on Windows (WARP) and macOS
//! (Metal).

use std::path::Path;

use atd_core::graph::{Edit, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::json;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
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
/// A temporary file, deleted when dropped (also when a test fails).
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

fn bursts() -> TempFile {
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
    TempFile(path)
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
    let example = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/audio-reactive-feedback.atidarla");
    let project = atd_format::load(&example).unwrap();

    // Saved to a new folder, the files are byte for byte the same.
    let copy = std::env::temp_dir().join(format!("atd-ac1-{}.atidarla", std::process::id()));
    let _ = std::fs::remove_dir_all(&copy);
    atd_format::save(&copy, &project).unwrap();
    for file in ["project.toml", "networks/main.toml"] {
        assert_eq!(
            std::fs::read(copy.join(file)).unwrap(),
            std::fs::read(example.join(file)).unwrap(),
            "{file} round-trips byte for byte"
        );
    }
    let reloaded = atd_format::load(&copy).unwrap();
    std::fs::remove_dir_all(&copy).ok();
    assert_eq!(reloaded, project);

    for (e, p, dir) in [(&mut a, &project, &example), (&mut b, &reloaded, &example)] {
        e.set_project_dir(Some(dir.clone()));
        e.load(p.main().unwrap().clone()).unwrap();
    }
    assert_eq!(a.network(), b.network());
    let node = |e: &Engine, name: &str| e.network().node_by_name(name).unwrap().id;
    let (audio, out) = (node(&a, "audio1"), node(&a, "level1"));

    // Tone bursts instead of the drum loop give a clear on/off to measure;
    // paused until both engines have decoded them.
    let wav = bursts();
    for e in [&mut a, &mut b] {
        e.edit(
            "test audio",
            [
                Edit::SetParam {
                    node: audio,
                    param: "file".into(),
                    value: Some(Param::constant(json!(wav.0.to_string_lossy()))),
                },
                Edit::SetParam {
                    node: audio,
                    param: "play".into(),
                    value: Some(Param::constant(json!(false))),
                },
            ],
        )
        .unwrap();
    }

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
    // Loudness maps to brightness 0.35–1.6: dim in silence, bright on a tone.
    eprintln!("mean brightness: {on:.1} with tone, {off:.1} in silence");
    assert!(
        on > 2.0 * off && on - off > 20.0,
        "the image follows the audio: {on:.1} with tone, {off:.1} in silence"
    );
}
