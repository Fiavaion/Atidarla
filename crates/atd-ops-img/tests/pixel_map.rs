//! Pixel Map on the GPU (FR-S7): a known gradient sampled through a patch
//! gives the expected DMX bytes, as channels and as Art-Net and sACN
//! packets received on loopback (the "test fixture" of M3 AC1).

use std::net::UdpSocket;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use serde_json::{Value, json};

/// One GPU test at a time (see `analyze.rs`).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const W: u32 = 64;
const H: u32 = 16;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: W,
        height: H,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn require_gpu() {
    let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(
        !required,
        "ATD_REQUIRE_GPU is set but there is no GPU adapter"
    );
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name("n");
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty, name),
    }];
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits).unwrap();
    id
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn project(name: &str, patch: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("atd-pixel-map-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fixtures")).unwrap();
    std::fs::write(dir.join("patch.toml"), patch).unwrap();
    // A project-local fixture type: blue first, then green.
    std::fs::write(
        dir.join("fixtures/bg.toml"),
        "name = \"Blue-green test fixture\"\nchannels = [\"blue\", \"green\"]\n",
    )
    .unwrap();
    TempDir(dir)
}

/// A horizontal black→white ramp (img.ramp) and a pixel map over it.
fn setup(e: &mut Engine, dir: &std::path::Path, params: &[(&str, Value)]) -> (NodeId, NodeId) {
    e.set_project_dir(Some(dir.to_owned()));
    let ramp = add(e, "img.ramp", &[]);
    let mut p = vec![("patch", json!("patch.toml"))];
    p.extend(params.iter().cloned());
    let map = add(e, "img.pixel_map", &p);
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(ramp, "out"), Endpoint::new(map, "in")),
        }],
    )
    .unwrap();
    (ramp, map)
}

/// Cooks until the first measurement arrives (the ramp does not change, so
/// it is final; the node keeps cooking to watch its patch file).
fn measure(e: &mut Engine, node: NodeId) -> Vec<(String, f32)> {
    let start = Instant::now();
    loop {
        e.cook(&[node], 0.0).unwrap();
        assert_eq!(e.error(node), None);
        let ch = e.channels(node).unwrap();
        let measured = ch.names().count() > 0 && ch.names().any(|n| ch.value(n) != Some(0.0));
        if measured {
            return ch
                .names()
                .map(|n| (n.to_owned(), ch.value(n).unwrap()))
                .collect();
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "pixel map measured (error: {:?})",
            e.error(node)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The patch under test: an 8-pixel RGB strip along the middle row, then a
/// project fixture (blue, green) at the right edge.
const PATCH: &str = r#"
[[fixture]]
id = "strip"
type = "pixel-rgb"
universe = 1
address = 1
count = 8
line = [[0.0, 0.5], [1.0, 0.5]]

[[fixture]]
id = "edge"
type = "bg"
universe = 1
address = 25
point = [0.99, 0.5]
"#;

/// The expected DMX bytes, from the ramp image read back on the CPU and
/// averaged as the shader does (4 × 4 texels over each cell).
fn expected(ramp: &[u8]) -> Vec<u8> {
    let texel = |x: f32, y: f32| {
        let px = ((x * W as f32).floor() as i64).clamp(0, i64::from(W) - 1) as usize;
        let py = ((y * H as f32).floor() as i64).clamp(0, i64::from(H) - 1) as usize;
        let i = (py * W as usize + px) * 4;
        [ramp[i], ramp[i + 1], ramp[i + 2]].map(|v| f32::from(v) / 255.0)
    };
    let avg = |cx: f32, cy: f32, hw: f32, hh: f32| {
        let mut sum = [0.0f32; 3];
        for y in 0..4 {
            for x in 0..4 {
                let ox = (x as f32 + 0.5) * 0.5 - 1.0;
                let oy = (y as f32 + 0.5) * 0.5 - 1.0;
                let t = texel(cx + ox * hw, cy + oy * hh);
                for c in 0..3 {
                    sum[c] += t[c] / 16.0;
                }
            }
        }
        sum
    };
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut out = Vec::new();
    for i in 0..8 {
        let c = avg((i as f32 + 0.5) / 8.0, 0.5, 1.0 / 16.0, 0.0);
        out.extend(c.map(byte));
    }
    let c = avg(0.99, 0.5, 0.0, 0.0);
    out.extend([byte(c[2]), byte(c[1])]);
    out
}

fn ramp_image(e: &Engine, ramp: NodeId) -> Vec<u8> {
    atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(ramp).unwrap()).unwrap()
}

fn close(got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(g.abs_diff(*w) <= 1, "slot {}: {g} vs {w} ({got:?})", i + 1);
    }
}

#[test]
fn gradient_gives_the_expected_dmx_channels() {
    let _serial = serial();
    let Some(mut e) = engine() else {
        return require_gpu();
    };
    let dir = project("channels", PATCH);
    let (ramp, map) = setup(&mut e, &dir.0, &[]);
    let values = measure(&mut e, map);
    let names: Vec<&str> = values.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names.len(), 26, "slots 1–26 of universe 1");
    assert_eq!((names[0], names[25]), ("u1_1", "u1_26"));
    let got: Vec<u8> = values
        .iter()
        .map(|(_, v)| (v * 255.0).round() as u8)
        .collect();
    // Slots 1–24 are the strip, 25–26 the project fixture.
    close(&got, &expected(&ramp_image(&e, ramp)));
    // A gradient: rising across the strip, dark at the left, bright right.
    assert!(got[0] < 40 && got[21] > 215, "{got:?}");
    assert!(
        got.chunks(3)
            .take(8)
            .zip(got.chunks(3).skip(1))
            .all(|(a, b)| a[0] <= b[0])
    );
}

/// Receives one DMX packet on `rx` and returns its slot data, parsed as
/// Art-Net (ArtDmx) or sACN (E1.31) by its header.
fn receive(rx: &UdpSocket) -> (u16, Vec<u8>) {
    let mut buf = [0u8; 700];
    let n = rx.recv(&mut buf).expect("a DMX packet on loopback");
    let p = &buf[..n];
    if p.starts_with(b"Art-Net\0") {
        assert_eq!(u16::from_le_bytes([p[8], p[9]]), 0x5000, "OpDmx");
        let universe = u16::from(p[14]) | (u16::from(p[15]) << 8);
        let len = usize::from(u16::from_be_bytes([p[16], p[17]]));
        (universe, p[18..18 + len].to_vec())
    } else {
        assert_eq!(&p[4..16], b"ASC-E1.17\0\0\0", "an E1.31 packet");
        let universe = u16::from_be_bytes([p[113], p[114]]);
        let count = usize::from(u16::from_be_bytes([p[123], p[124]]));
        assert_eq!(p[125], 0, "DMX start code");
        (universe, p[126..125 + count].to_vec())
    }
}

#[test]
fn loopback_fixture_receives_artnet_and_sacn() {
    let _serial = serial();
    if engine().is_none() {
        return require_gpu();
    }
    for protocol in ["artnet", "sacn"] {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let port = rx.local_addr().unwrap().port();
        let dir = project(protocol, PATCH);
        let mut e = engine().unwrap();
        let (ramp, map) = setup(
            &mut e,
            &dir.0,
            &[
                ("protocol", json!(protocol)),
                ("host", json!("127.0.0.1")),
                ("port", json!(port)),
            ],
        );
        measure(&mut e, map);
        let want = expected(&ramp_image(&e, ramp));
        // The first packets may carry the universe before the first
        // measurement (zeros); the fixture settles on the image.
        let start = Instant::now();
        loop {
            let (universe, data) = receive(&rx);
            assert_eq!(universe, 1, "{protocol}");
            let slots = &data[..want.len()];
            if slots.iter().any(|v| *v != 0) {
                close(slots, &want);
                break;
            }
            assert!(
                start.elapsed().as_secs() < 10,
                "{protocol}: image reached the fixture"
            );
        }
    }
}

#[test]
fn patch_errors_are_node_errors() {
    let _serial = serial();
    let Some(mut e) = engine() else {
        return require_gpu();
    };
    let dir = project(
        "errors",
        "[[fixture]]\nid = \"a\"\ntype = \"nope\"\npoint = [0, 0]\n",
    );
    let (_, map) = setup(&mut e, &dir.0, &[]);
    let start = Instant::now();
    let err = loop {
        e.cook(&[map], 0.0).unwrap();
        if let Some(err) = e.error(map) {
            break err.to_owned();
        }
        assert!(
            start.elapsed().as_secs() < 30,
            "the patch error is reported"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        err.contains("patch.toml") && err.contains("no fixture type `nope`"),
        "{err}"
    );
}
