//! The M6 capture and lookup operators on the GPU: points from images and
//! depth, attributes from images and channels, point files and Sensor In's
//! test source. Skips without a GPU unless `ATD_REQUIRE_GPU` is set.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::read_blocking;
use serde_json::{Value, json};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()
}

/// An engine with the PNT, IMG and SIG operators, rendering `w` × `h`
/// images (IMG generators make images of that size).
fn engine(w: u32, h: u32) -> Option<Engine> {
    let (device, queue) = gpu()?;
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    atd_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: w,
        height: h,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

struct Build<'a> {
    e: &'a mut Engine,
    edits: Vec<Edit>,
}

impl<'a> Build<'a> {
    fn new(e: &'a mut Engine) -> Self {
        Self {
            e,
            edits: Vec::new(),
        }
    }

    fn node(&mut self, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let id = self.e.alloc_id();
        self.edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", id.0)),
        });
        for (p, v) in params {
            self.edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
        id
    }

    fn wire(&mut self, from: NodeId, to: NodeId, socket: &str) {
        self.edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
        });
    }

    fn done(self) {
        self.e.edit("build", self.edits).unwrap();
    }
}

/// Point attribute `name` of `node` as rows of its components.
fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<Vec<f32>> {
    assert_eq!(e.error(node), None);
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    let bytes = read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap();
    let stride = a.ty().stride() as usize;
    let comps = a.ty().comps() as usize;
    bytes
        .chunks_exact(stride)
        .map(|c| {
            (0..comps)
                .map(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))
                .collect()
        })
        .collect()
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn points_from_image_lay_a_coloured_grid_over_the_image() {
    let Some(mut e) = engine(16, 8) else { return };
    let mut b = Build::new(&mut e);
    let ramp = b.node("img.ramp", &[]);
    let pts = b.node(
        "pnt.points_from_image",
        &[
            ("columns", json!(4)),
            ("displace", json!(1.0)),
            ("bilinear", json!(false)),
        ],
    );
    b.wire(ramp, pts, "image");
    b.done();
    e.cook(&[pts], 0.0).unwrap();
    let g = e.geometry(pts).unwrap();
    assert_eq!(g.points(), 8, "4 columns × 2 rows (16 × 8 image)");
    let p = floats(&e, pts, "P");
    let cd = floats(&e, pts, "Cd");
    let v = floats(&e, pts, "value");
    let uv = floats(&e, pts, "uv");
    for i in 0..8 {
        let (c, r) = (i % 4, i / 4);
        let x = (c as f32 + 0.5) / 4.0;
        let y = (r as f32 + 0.5) / 2.0;
        assert!(close(uv[i][0], x, 1e-6) && close(uv[i][1], y, 1e-6));
        assert!(close(p[i][0], (x - 0.5) * 2.0, 1e-5), "{:?}", p[i]);
        assert!(close(p[i][1], y - 0.5, 1e-5), "height follows the aspect");
        // A horizontal ramp, black to white: the pixel at x.
        assert!(close(cd[i][0], x, 0.04), "{} at {x}", cd[i][0]);
        assert!(close(v[i][0], cd[i][0], 1e-3), "luminance of grey");
        assert!(close(p[i][2], v[i][0], 1e-5), "displaced by the value");
    }
    assert_eq!(floats(&e, pts, "id").len(), 8, "every point has an id");
}

#[test]
fn points_from_depth_back_project_and_mark_missing_depth() {
    let Some(mut e) = engine(8, 8) else { return };
    let mut b = Build::new(&mut e);
    let grey = b.node("img.constant", &[("colour", json!([0.5, 0.5, 0.5, 1.0]))]);
    let black = b.node("img.constant", &[]);
    let pts = b.node(
        "pnt.points_from_depth",
        &[
            ("columns", json!(8)),
            ("encoding", json!("linear")),
            ("near", json!(1.0)),
            ("far", json!(3.0)),
            ("fov", json!(90.0)),
        ],
    );
    let none = b.node("pnt.points_from_depth", &[("columns", json!(4))]);
    b.wire(grey, pts, "depth");
    b.wire(black, none, "depth");
    b.done();
    e.cook(&[pts, none], 0.0).unwrap();
    assert_eq!(e.geometry(pts).unwrap().points(), 64);
    let p = floats(&e, pts, "P");
    let d = floats(&e, pts, "depth");
    let ok = floats(&e, pts, "valid");
    // 0.5 of 8 bits is 128 / 255.
    let depth = 1.0 + 2.0 * (128.0 / 255.0);
    for i in 0..64 {
        assert!(close(d[i][0], depth, 1e-4), "{}", d[i][0]);
        assert_eq!(ok[i][0], 1.0);
        assert!(close(p[i][2], -depth, 1e-4), "looking down -Z");
    }
    // A 90° vertical field of view: the image's half-height is the depth.
    // The top-right cell (row 7, column 7) is half a cell from the corner.
    let corner = p[63].clone();
    let edge = depth * (1.0 - 1.0 / 8.0);
    assert!(
        close(corner[0], edge, 1e-3) && close(corner[1], edge, 1e-3),
        "{corner:?}"
    );
    // Zero depth is missing: invalid and transparent.
    let ok = floats(&e, none, "valid");
    let cd = floats(&e, none, "Cd");
    assert!(ok.iter().all(|v| v[0] == 0.0));
    assert!(cd.iter().all(|c| c[3] == 0.0));
}

#[test]
fn attribute_from_image_projects_points_onto_an_image() {
    let Some(mut e) = engine(32, 32) else { return };
    let mut b = Build::new(&mut e);
    let ramp = b.node("img.ramp", &[]);
    let pts = b.node("pnt.points", &[("count", json!(100))]);
    let look = b.node(
        "pnt.attribute_from_image",
        &[
            ("lookup", json!("planar")),
            ("name", json!("shade")),
            ("type", json!("float")),
        ],
    );
    let scaled = b.node(
        "pnt.attribute_from_image",
        &[
            ("lookup", json!("planar")),
            ("operation", json!("multiply")),
            ("amount", json!(0.5)),
        ],
    );
    b.wire(pts, look, "in");
    b.wire(ramp, look, "image");
    b.wire(look, scaled, "in");
    b.wire(ramp, scaled, "image");
    b.done();
    e.cook(&[scaled], 0.0).unwrap();
    let p = floats(&e, scaled, "P");
    let shade = floats(&e, scaled, "shade");
    let cd = floats(&e, scaled, "Cd");
    for i in 0..100 {
        let x = (p[i][0] + 1.0) / 2.0;
        assert!(close(shade[i][0], x, 0.05), "{} at {x}", shade[i][0]);
        // White × ramp, blended halfway: 1 - (1 - x) / 2.
        assert!(close(cd[i][0], 0.5 + 0.5 * x, 0.05), "{} at {x}", cd[i][0]);
        assert!(close(cd[i][3], 1.0, 1e-3), "alpha 1 × 1");
    }
    // Offline the same (no worker threads involved).
    e.set_mode(CookMode::Offline { fps: 60.0 });
    e.cook(&[scaled], 1.0).unwrap();
    assert_eq!(floats(&e, scaled, "shade"), shade);
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("atd-pnt-capture-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A cloud of `n` points along X at height `y`, with a colour and a
/// float attribute.
fn cloud(n: usize, y: f32) -> atd_ops_pnt::cache::Cloud {
    use atd_ops_pnt::cache::{Cloud, Column};
    let p: Vec<f32> = (0..n).flat_map(|i| [i as f32, y, 0.0]).collect();
    let cd: Vec<f32> = (0..n).flat_map(|_| [1.0, 0.5, 0.25, 1.0]).collect();
    let heat: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();
    Cloud {
        count: n,
        columns: vec![
            Column::floats("P", 3, &p),
            Column::floats("Cd", 4, &cd),
            Column::floats("heat", 1, &heat),
        ],
    }
}

#[test]
fn point_file_in_reads_files_and_sequences_on_a_worker() {
    use atd_ops_pnt::cache::{Format, write};
    let Some(mut e) = engine(8, 8) else { return };
    let dir = scratch("file");
    for f in 1..=3 {
        let bytes = write(&cloud(4 + f, f as f32), Format::Atdpc).unwrap();
        std::fs::write(dir.join(format!("seq.{f:04}.atdpc")), bytes).unwrap();
    }
    std::fs::write(
        dir.join("table.csv"),
        "x,y,z,red,green,blue,heat\n0,0,0,1,0,0,2\n1,2,3,0,1,0,4\n",
    )
    .unwrap();
    e.set_project_dir(Some(dir.clone()));
    let mut b = Build::new(&mut e);
    let seq = b.node(
        "pnt.file_in",
        &[("file", json!("seq.$F4.atdpc")), ("frame", json!(2))],
    );
    let csv = b.node("pnt.file_in", &[("file", json!("table.csv"))]);
    let missing = b.node("pnt.file_in", &[("file", json!("nothing.ply"))]);
    b.done();
    // Live: the file arrives on a later frame; nothing blocks.
    let mut t = 0.0;
    for _ in 0..500 {
        e.cook(&[seq, csv, missing], t).unwrap();
        t += 1.0 / 60.0;
        let ready = [seq, csv]
            .iter()
            .all(|n| e.geometry(*n).is_some_and(|g| g.points() > 0));
        if ready && e.error(missing).is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(e.geometry(seq).unwrap().points(), 6, "frame 2: 6 points");
    let p = floats(&e, seq, "P");
    assert_eq!(p[5], vec![5.0, 2.0, 0.0]);
    assert_eq!(floats(&e, seq, "heat")[3], vec![1.5]);
    assert_eq!(floats(&e, csv, "Cd")[1], vec![0.0, 1.0, 0.0, 1.0]);
    assert_eq!(floats(&e, csv, "heat")[1], vec![4.0]);
    assert_eq!(e.geometry(csv).unwrap().points(), 2);
    assert!(
        e.error(missing).unwrap().contains("nothing.ply"),
        "{:?}",
        e.error(missing)
    );
    // Offline, frame by frame: each cook waits for its own file.
    e.set_mode(CookMode::Offline { fps: 60.0 });
    for f in [3i64, 1, 2] {
        e.edit(
            "frame",
            vec![Edit::SetParam {
                node: seq,
                param: "frame".into(),
                value: Some(Param::constant(json!(f))),
            }],
        )
        .unwrap();
        e.cook(&[seq], t).unwrap();
        t += 1.0 / 60.0;
        assert_eq!(e.geometry(seq).unwrap().points(), 4 + f as u32);
        assert_eq!(floats(&e, seq, "P")[0][1], f as f32);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sensor_in_test_scene_sees_a_figure_and_tracks_its_joints() {
    let Some(mut e) = engine(8, 8) else { return };
    let mut b = Build::new(&mut e);
    let depth = b.node("pnt.sensor_in", &[("columns", json!(80))]);
    let body = b.node("pnt.sensor_in", &[("output", json!("body"))]);
    let live = b.node("pnt.sensor_in", &[("device", json!("k4a"))]);
    b.done();
    e.cook(&[depth, body, live], 0.5).unwrap();
    let g = e.geometry(depth).unwrap();
    assert_eq!(g.points(), 80 * 72, "320 × 288 depth, 80 columns");
    let d = floats(&e, depth, "depth");
    let p = floats(&e, depth, "P");
    let ok = floats(&e, depth, "valid");
    let g = e.geometry(depth).unwrap();
    let bodies = read_blocking(
        e.device(),
        e.queue(),
        e.buffer_pool(),
        g.point("body").unwrap(),
    )
    .unwrap()
    .chunks_exact(4)
    .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
    .collect::<Vec<_>>();
    let on_figure = bodies.iter().filter(|b| **b == 0).count();
    assert!(
        on_figure > 100,
        "the figure covers part of the view: {on_figure}"
    );
    for i in 0..d.len() {
        assert_eq!(ok[i][0], 1.0, "everything is between 0.5 and 4 m");
        assert!((p[i][2] + d[i][0]).abs() < 1e-4, "P.z is −depth");
        if bodies[i] == 0 {
            assert!(d[i][0] > 1.8 && d[i][0] < 2.6, "{}", d[i][0]);
        } else {
            // The floor meets the bottom of a 65° view at about 1.57 m.
            assert!(d[i][0] > 1.5 && d[i][0] < 3.7, "wall or floor: {}", d[i][0]);
        }
    }
    let joints = floats(&e, body, "P");
    assert_eq!(joints.len(), 32);
    let pose = atd_ops_pnt::sensor_test_pose(0.5);
    for (j, q) in joints.iter().zip(pose) {
        assert!(j.iter().zip(q).all(|(a, b)| (a - b).abs() < 1e-5));
    }
    assert_eq!(e.geometry(body).unwrap().prims(), 31, "bones as lines");
    let err = e.error(live).unwrap();
    assert!(err.contains("K4A") || err.contains("k4a"), "{err}");
    // The test scene is a function of time and frame: an offline repeat
    // gives the same points.
    e.set_mode(CookMode::Offline { fps: 60.0 });
    e.set_frame(10);
    e.cook(&[depth], 1.0).unwrap();
    let a = floats(&e, depth, "P");
    e.set_frame(10);
    e.cook(&[depth], 1.0).unwrap();
    assert_eq!(floats(&e, depth, "P"), a);
    // Offline pooled attributes are cleared in the frame's encoder; the
    // joints still arrive.
    e.cook(&[body], 1.0).unwrap();
    let pose = atd_ops_pnt::sensor_test_pose(1.0);
    for (j, q) in floats(&e, body, "P").iter().zip(pose) {
        assert!(j.iter().zip(q).all(|(a, b)| (a - b).abs() < 1e-5));
    }
}
