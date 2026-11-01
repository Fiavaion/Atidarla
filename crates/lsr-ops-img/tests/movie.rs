//! Movie In on the GPU against FFmpeg's decode of the same HAP clips
//! (`lsr-io/tests/data`), with BC textures where the adapter has them and
//! with the CPU fallback.

use std::path::PathBuf;

use lsr_core::graph::{Edit, Node, NodeId, Param};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_interop::readback::read_rgba8;
use serde_json::{Value, json};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../lsr-io/tests/data")
        .join(name)
}

/// An engine at the clips' size (64×32), with or without BC textures.
/// `None` when there is no adapter, or BC was asked for and is missing.
fn engine(bc: bool) -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
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
    lsr_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn require_gpu() {
    let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let has_adapter = pollster::block_on(
        lsr_gpu::primary_instance().request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .is_ok();
    assert!(
        !required || has_adapter,
        "LSR_REQUIRE_GPU is set but there is no GPU adapter"
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
        // GPUs decode BC1/BC3 with their own interpolation: NVIDIA is up to 3
        // levels from FFmpeg where the CPU path is within 2.
        let tolerance = tolerance + if bc { 2 } else { 0 };
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

#[test]
fn stills_load_as_a_single_frame() {
    let Some(mut e) = engine(false) else {
        require_gpu();
        return;
    };
    let node = movie(&mut e, "still.png", &[]);
    let got = frame(&mut e, node, 0.0);
    let want = std::fs::read(data("hap1.f2.rgba")).unwrap();
    // The PNG is lossless; the HAP reference is BC1, hence the tolerance.
    let mean = got
        .iter()
        .zip(&want)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / want.len() as f64;
    assert!(mean < 6.0, "mean difference {mean}");
    // Time does not change a still.
    assert_eq!(frame(&mut e, node, 3.0), got);
}

#[test]
fn native_size_follows_the_file() {
    let Some(mut e) = engine(false) else {
        require_gpu();
        return;
    };
    // A 20×10 still, unlike the 64×32 engine.
    let path = std::env::temp_dir().join(format!("lsr-native-{}.png", std::process::id()));
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 20, 10);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&[200u8; 20 * 10 * 4])
            .unwrap();
    }
    let id = e.alloc_id();
    e.edit(
        "add",
        [
            Edit::AddNode {
                node: Node::new(id, "img.movie_in", "still"),
            },
            Edit::SetParam {
                node: id,
                param: "file".into(),
                value: Some(Param::constant(json!(path.to_string_lossy()))),
            },
        ],
    )
    .unwrap();
    let size = |e: &mut Engine| {
        let start = std::time::Instant::now();
        loop {
            e.cook(&[id], 0.0).unwrap();
            let t = e.result(id).unwrap();
            if (t.width(), t.height()) == (20, 10) || start.elapsed().as_secs() > 5 {
                return (t.width(), t.height());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    assert_eq!(size(&mut e), (20, 10), "native by default");
    e.edit(
        "size",
        [Edit::SetParam {
            node: id,
            param: "size".into(),
            value: Some(Param::constant(json!("project"))),
        }],
    )
    .unwrap();
    e.cook(&[id], 0.0).unwrap();
    let t = e.result(id).unwrap();
    assert_eq!((t.width(), t.height()), (64, 32), "project size");
    std::fs::remove_file(path).ok();
}

#[test]
fn relative_paths_are_relative_to_the_project_folder() {
    let Some(mut e) = engine(false) else {
        require_gpu();
        return;
    };
    // The project folder is the test data folder: "hap1.mov" is in it.
    e.set_project_dir(Some(data("")));
    let id = e.alloc_id();
    e.edit(
        "add",
        [
            Edit::AddNode {
                node: Node::new(id, "img.movie_in", "movie1"),
            },
            Edit::SetParam {
                node: id,
                param: "file".into(),
                value: Some(Param::constant(json!("hap1.mov"))),
            },
            Edit::SetParam {
                node: id,
                param: "cue".into(),
                value: Some(Param::constant(json!(0.0))),
            },
        ],
    )
    .unwrap();
    frame(&mut e, id, 0.0);
    let later = frame(&mut e, id, 0.25);
    let want = std::fs::read(data("hap1.f2.rgba")).unwrap();
    assert!(worst(&later, &want) <= 2, "the relative file plays");
}
