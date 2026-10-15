//! Movie In on the GPU against FFmpeg's decode of the same HAP clips
//! (`atd-io/tests/data`), with BC textures where the adapter has them and
//! with the CPU fallback.

use std::path::PathBuf;

use atd_core::graph::{Edit, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../atd-io/tests/data")
        .join(name)
}

/// An engine at the clips' size (64×32), with or without BC textures.
/// `None` when there is no adapter, or BC was asked for and is missing.
fn engine(bc: bool) -> Option<Engine> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let features = if bc {
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            eprintln!("adapter has no BC texture compression; GPU path skipped");
            return None;
        }
        wgpu::Features::TEXTURE_COMPRESSION_BC
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        ..Default::default()
    }))
    .ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn require_gpu() {
    let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let has_adapter = pollster::block_on(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
            .request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .is_ok();
    assert!(
        !required || has_adapter,
        "ATD_REQUIRE_GPU is set but there is no GPU adapter"
    );
}

fn movie(e: &mut Engine, file: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, "img.movie_in", "movie1"),
    }];
    let path = data(file).to_string_lossy().into_owned();
    for (p, v) in [("file", json!(path))].iter().chain(params) {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits).unwrap();
    id
}

/// Cooks until the requested frame has been uploaded, then reads it back.
fn frame(e: &mut Engine, node: NodeId, time: f64) -> Vec<u8> {
    let start = std::time::Instant::now();
    let mut last = Vec::new();
    // A frame is "ready" once the output stops changing between cooks.
    loop {
        e.cook(&[node], time).unwrap();
        assert_eq!(e.error(node), None);
        let px = read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap();
        if px == last && px.iter().any(|v| *v != 0) {
            return px;
        }
        last = px;
        assert!(start.elapsed().as_secs() < 10, "frame never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
}

fn check(bc: bool) {
    let Some(mut e) = engine(bc) else {
        require_gpu();
        return;
    };
    for (name, tolerance) in [
        ("hap1", 2),
        ("hap1_chunked", 2),
        ("hap_alpha", 2),
        ("hap_q", 6),
    ] {
        let node = movie(
            &mut e,
            &format!("{name}.mov"),
            &[("play", json!(false)), ("cue", json!(0.25))],
        );
        let got = frame(&mut e, node, 0.0);
        let want = std::fs::read(data(&format!("{name}.f2.rgba"))).unwrap();
        let d = worst(&got, &want);
        assert!(d <= tolerance, "{name} (bc: {bc}): max difference {d}");
        e.edit("remove", [Edit::RemoveNode { id: node }]).unwrap();
    }
}

#[test]
fn hap_on_gpu_textures_matches_ffmpeg() {
    check(true);
}

#[test]
fn hap_through_cpu_fallback_matches_ffmpeg() {
    check(false);
}

#[test]
fn playback_advances_and_loops() {
    let Some(mut e) = engine(false) else {
        require_gpu();
        return;
    };
    let node = movie(&mut e, "hap1.mov", &[("cue", json!(0.0))]);
    let first = frame(&mut e, node, 0.0);
    // 0.25 s of playback lands on frame 2, which differs from frame 0.
    let later = frame(&mut e, node, 0.25);
    let want = std::fs::read(data("hap1.f2.rgba")).unwrap();
    assert!(worst(&later, &want) <= 2, "playing reaches frame 2");
    assert!(worst(&first, &later) > 20, "frames differ");
    // One second further wraps around to frame 2 again.
    let wrapped = frame(&mut e, node, 1.25);
    assert!(worst(&wrapped, &want) <= 2, "looping wraps");

    set_file(&mut e, node, "missing.mov");
    let start = std::time::Instant::now();
    loop {
        e.cook(&[node], 2.0).unwrap();
        if e.error(node).is_some() {
            break;
        }
        assert!(start.elapsed().as_secs() < 10, "missing file reported");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn set_file(e: &mut Engine, node: NodeId, file: &str) {
    let path = data(file).to_string_lossy().into_owned();
    e.edit(
        "file",
        [Edit::SetParam {
            node,
            param: "file".into(),
            value: Some(Param::constant(json!(path))),
        }],
    )
    .unwrap();
}
