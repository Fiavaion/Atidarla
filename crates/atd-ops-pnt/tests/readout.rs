//! To Channels and Pixel Out on the GPU (PRD A65): known points read back
//! asynchronously give the expected channels and DMX slots, live (a frame
//! or two late) and offline (the frame's own points). Skips without a GPU
//! unless `ATD_REQUIRE_GPU` is set.

use std::net::UdpSocket;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::read_blocking;
use serde_json::{Value, json};

/// One GPU test at a time (readbacks of parallel software devices stall;
/// see `atd-ops-img/tests/analyze.rs`).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()?;
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: 16,
        height: 16,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty, format!("n{}", id.0)),
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

fn set(e: &mut Engine, node: NodeId, param: &str, v: Value) {
    e.edit(
        "set",
        [Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param::constant(v)),
        }],
    )
    .unwrap();
}

fn wire(e: &mut Engine, from: NodeId, to: NodeId) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, "in")),
        }],
    )
    .unwrap();
}

/// Point attribute `name` of `node` as rows of floats (the reference).
fn rows(e: &Engine, node: NodeId, name: &str) -> Vec<Vec<f32>> {
    let a = e.geometry(node).unwrap().point(name).unwrap();
    let bytes = read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap();
    let (stride, comps) = (a.ty().stride() as usize, a.ty().comps() as usize);
    bytes
        .chunks_exact(stride)
        .map(|c| {
            (0..comps)
                .map(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))
                .collect()
        })
        .collect()
}

/// Live: cooks until the readback has arrived and nothing is in flight.
fn settle(e: &mut Engine, node: NodeId) {
    let start = Instant::now();
    let mut cooks = 0;
    loop {
        e.cook(&[node], 0.0).unwrap();
        cooks += 1;
        let has = e.channels(node).is_some_and(|c| !c.is_empty());
        if cooks > 2 && has && !e.pending() {
            return;
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "readback finished (error: {:?})",
            e.error(node)
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn channels(e: &Engine, node: NodeId) -> Vec<(String, Vec<f32>)> {
    let ch = e.channels(node).unwrap();
    (0..ch.len())
        .map(|c| (ch.name(c).to_owned(), ch.channel(c).to_vec()))
        .collect()
}

#[test]
fn to_channels_reads_known_points_per_point_and_as_samples() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let pts = add(
        &mut e,
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(10)),
            ("random_colour", json!(true)),
        ],
    );
    let out = add(&mut e, "pnt.to_channels", &[("max", json!(6))]);
    wire(&mut e, pts, out);
    settle(&mut e, out);
    assert_eq!(e.error(out), None);
    let p = rows(&e, pts, "P");
    let got = channels(&e, out);
    // Six points (the cap) of three components, point-major.
    assert_eq!(got.len(), 18);
    assert_eq!(got[0].0, "P0_x");
    assert_eq!(got[4].0, "P1_y");
    assert_eq!(got[17].0, "P5_z");
    for (k, (_, v)) in got.iter().enumerate() {
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], p[k / 3][k % 3], "channel {k}");
    }

    // Samples layout: one channel per component, a sample per point.
    set(&mut e, out, "attr", json!("Cd"));
    set(&mut e, out, "layout", json!("samples"));
    set(&mut e, out, "max", json!(100));
    settle(&mut e, out);
    let cd = rows(&e, pts, "Cd");
    let got = channels(&e, out);
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Cd_r", "Cd_g", "Cd_b", "Cd_a"]);
    for (c, (_, v)) in got.iter().enumerate() {
        let want: Vec<f32> = cd.iter().map(|r| r[c]).collect();
        assert_eq!(v, &want, "component {c}");
    }

    // A one-component attribute has no suffix; a missing one is an error.
    set(&mut e, out, "attr", json!("id"));
    set(&mut e, out, "layout", json!("points"));
    set(&mut e, out, "max", json!(3));
    settle(&mut e, out);
    let ids = channels(&e, out);
    let names: Vec<&str> = ids.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["id0", "id1", "id2"]);
    assert_eq!(ids[2].1, [2.0]);
    set(&mut e, out, "attr", json!("nothing"));
    e.cook(&[out], 0.0).unwrap();
    assert!(
        e.error(out).is_some_and(|m| m.contains("`nothing`")),
        "{:?}",
        e.error(out)
    );
    assert!(e.channels(out).unwrap().is_empty());
}

#[test]
fn to_channels_offline_reads_each_frames_own_points() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    e.set_mode(CookMode::Offline { fps: 30.0 });
    let pts = add(
        &mut e,
        "pnt.points",
        &[("shape", json!("random")), ("count", json!(4))],
    );
    let out = add(&mut e, "pnt.to_channels", &[]);
    wire(&mut e, pts, out);
    for (frame, centre) in [[0.0, 0.0, 0.0], [5.0, -2.0, 1.0], [9.0, 9.0, 9.0]]
        .into_iter()
        .enumerate()
    {
        set(&mut e, pts, "centre", json!(centre));
        // One cook per frame: no waiting for a later one.
        e.cook(&[out], frame as f64 / 30.0).unwrap();
        assert_eq!(e.error(out), None);
        let p = rows(&e, pts, "P");
        let got = channels(&e, out);
        assert_eq!(got.len(), 12, "frame {frame}");
        for (k, (_, v)) in got.iter().enumerate() {
            assert_eq!(v[0], p[k / 3][k % 3], "frame {frame}, channel {k}");
        }
    }
}

#[test]
fn to_channels_stops_at_the_live_count_of_compacted_points() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let pts = add(
        &mut e,
        "pnt.points",
        &[("shape", json!("line")), ("count", json!(10))],
    );
    // Keeps ids 2, 3 and 4: a GPU-side count of 3 in buffers of 10.
    let keep = add(
        &mut e,
        "pnt.subset",
        &[
            ("rule", json!("range")),
            ("first", json!(2)),
            ("count", json!(3)),
        ],
    );
    let out = add(&mut e, "pnt.to_channels", &[("attr", json!("id"))]);
    wire(&mut e, pts, keep);
    wire(&mut e, keep, out);
    settle(&mut e, out);
    assert_eq!(e.error(out), None);
    assert!(e.geometry(keep).unwrap().is_gpu_counted());
    let got = channels(&e, out);
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["id0", "id1", "id2"]);
    let mut ids: Vec<f32> = got.iter().map(|(_, v)| v[0]).collect();
    ids.sort_by(f32::total_cmp);
    assert_eq!(ids, [2.0, 3.0, 4.0]);
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Ten RGB pixels on universe 3 (the region is Pixel Map's, ignored here),
/// then a dimmer-only fixture.
const PATCH: &str = r#"
[[fixture]]
id = "strip"
type = "pixel-rgb"
universe = 3
address = 1
count = 10
line = [[0.0, 0.5], [1.0, 0.5]]

[[fixture]]
id = "dim"
type = "dimmer"
universe = 3
address = 31
point = [0.5, 0.5]
"#;

#[test]
fn pixel_out_maps_point_colours_to_dmx_slots_and_sends_them() {
    let _serial = serial();
    let Some(mut e) = engine() else { return };
    let dir = std::env::temp_dir().join(format!("atd-pixel-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("patch.toml"), PATCH).unwrap();
    let dir = TempDir(dir);
    e.set_project_dir(Some(dir.0.clone()));
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let port = rx.local_addr().unwrap().port();

    // Eight coloured points for eleven pixels: the last three are black.
    let pts = add(
        &mut e,
        "pnt.points",
        &[
            ("shape", json!("random")),
            ("count", json!(8)),
            ("random_colour", json!(true)),
        ],
    );
    let out = add(
        &mut e,
        "pnt.pixel_out",
        &[
            ("patch", json!("patch.toml")),
            ("protocol", json!("artnet")),
            ("host", json!("127.0.0.1")),
            ("port", json!(port)),
        ],
    );
    wire(&mut e, pts, out);
    let start = Instant::now();
    // Until the first readback has filled the slots (all zero before).
    while e
        .channels(out)
        .is_none_or(|c| (0..c.len()).all(|i| c.last(i) == 0.0))
    {
        e.cook_frame(None, 0.0).unwrap();
        assert!(
            start.elapsed().as_secs() < 60,
            "pixel out measured (error: {:?}, channels {:?})",
            e.error(out),
            e.channels(out)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(e.error(out), None);
    let cd = rows(&e, pts, "Cd");
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut want: Vec<u8> = cd
        .iter()
        .flat_map(|c| [c[0], c[1], c[2]])
        .map(byte)
        .collect();
    want.resize(31, 0);
    let got = channels(&e, out);
    assert_eq!(got.len(), 31, "slots 1–31 of universe 3");
    assert_eq!((got[0].0.as_str(), got[30].0.as_str()), ("u3_1", "u3_31"));
    let bytes: Vec<u8> = got
        .iter()
        .map(|(_, v)| (v[0] * 255.0).round() as u8)
        .collect();
    assert_eq!(bytes, want);

    // The same slots as Art-Net, through the DMX senders.
    let mut buf = [0u8; 700];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let n = rx.recv(&mut buf).expect("an Art-Net packet on loopback");
        let p = &buf[..n];
        assert!(p.starts_with(b"Art-Net\0"));
        let universe = u16::from(p[14]) | (u16::from(p[15]) << 8);
        let len = usize::from(u16::from_be_bytes([p[16], p[17]]));
        if universe == 3 && p[18..18 + len.min(31)] == want[..] {
            break;
        }
        assert!(Instant::now() < deadline, "the measured slots were sent");
    }
}
