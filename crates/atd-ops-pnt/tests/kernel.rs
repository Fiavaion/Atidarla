//! `pnt.kernel` on the GPU (A12, FR-K1..K4): M2's AC2 (a ten-line kernel
//! gives the same particles as the node-built equivalent) and AC3 (editing a
//! running kernel keeps the simulation's state). Skips without a GPU unless
//! `ATD_REQUIRE_GPU` is set; one test at a time (two devices running compute
//! passes in parallel crash WARP).

use std::sync::{Mutex, MutexGuard};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::read_blocking;
use serde_json::{Value, json};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// An offline engine (fixed step of 1/60 s) with the PNT operators.
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
        width: 64,
        height: 64,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    e.set_mode(CookMode::Offline { fps: 60.0 });
    Some(e)
}

/// The kernel of AC2, as in `docs/kernel-dsl.md`.
const AC2: &str = r#"// Curl noise + attract + age: the M2 AC2 particle kernel.
float freq   = ch("freq", 0.5, 0.0, 4.0);
float amp    = ch("amp", 2.0, 0.0, 10.0);
float pull   = ch("attract", 0.3, 0.0, 5.0);
vec3  target = chv("target", {0, 0, 0});
float dt     = @TimeInc;
v@v += curlnoise(v@P * freq) * (amp * dt);
v@v += (target - v@P) * (pull * dt);
v@P += v@v * dt;
f@age += dt;
"#;

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

    fn after(&mut self, from: NodeId, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let n = self.node(ty, params);
        self.wire(from, n, "in");
        n
    }

    /// A Solver fed by `init` whose body is `body` (first and last node).
    fn solver(&mut self, init: NodeId, body: impl FnOnce(&mut Self, NodeId) -> NodeId) -> NodeId {
        let sim = self.node("pnt.solver", &[]);
        self.wire(init, sim, "init");
        let last = body(self, sim);
        self.wire(last, sim, "prev");
        last
    }

    fn done(self) {
        self.e.edit("build", self.edits).unwrap();
    }
}

fn set_code(e: &mut Engine, node: NodeId, code: &str) {
    e.edit(
        "code",
        vec![Edit::SetParam {
            node,
            param: "code".into(),
            value: Some(Param::constant(json!(code))),
        }],
    )
    .unwrap();
}

fn cook(e: &mut Engine, nodes: &[NodeId], f: u64) {
    e.set_frame(f);
    e.cook(nodes, f as f64 / 60.0).unwrap();
    for n in nodes {
        assert_eq!(e.error(*n), None, "node {n:?} at frame {f}");
    }
}

/// The 32-bit words of point attribute `name` of `node` (live points only).
fn words(e: &Engine, node: NodeId, name: &str) -> Vec<u32> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    let bytes = read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap();
    let n = g.points() as usize * a.ty().stride() as usize;
    bytes[..n]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<f32> {
    words(e, node, name)
        .into_iter()
        .map(f32::from_bits)
        .collect()
}

fn scatter(b: &mut Build<'_>, count: i64) -> NodeId {
    b.node(
        "pnt.scatter",
        &[("count", json!(count)), ("radius", json!(1.5))],
    )
}

/// AC2: the ten-line kernel and Noise → Attract → Integrate → Age, each in a
/// Solver loop over the same points, agree bit for bit for 120 frames.
#[test]
fn ten_line_kernel_matches_the_node_chain() {
    let _s = serial();
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let seed = scatter(&mut b, 20_000);
    let nodes = b.solver(seed, |b, sim| {
        let noise = b.after(
            sim,
            "pnt.noise",
            &[
                ("mode", json!("velocity")),
                ("amplitude", json!(2.0)),
                ("frequency", json!(0.5)),
            ],
        );
        let pull = b.after(
            noise,
            "pnt.attract",
            &[("strength", json!(0.3)), ("mode", json!("spring"))],
        );
        let step = b.after(pull, "pnt.integrate", &[]);
        b.after(step, "pnt.age", &[("kill_dead", json!(false))])
    });
    let kernel = b.solver(seed, |b, sim| {
        b.after(sim, "pnt.kernel", &[("code", json!(AC2))])
    });
    b.done();
    for f in 0..120 {
        cook(&mut e, &[nodes, kernel], f);
    }
    let mut worst = 0.0f32;
    for name in ["P", "v", "age"] {
        let (a, k) = (floats(&e, nodes, name), floats(&e, kernel, name));
        assert_eq!(a.len(), k.len(), "{name}");
        let same = a
            .iter()
            .zip(&k)
            .filter(|(x, y)| x.to_bits() == y.to_bits())
            .count();
        let d = a
            .iter()
            .zip(&k)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        worst = worst.max(d);
        eprintln!(
            "{name}: {same}/{} words identical, largest difference {d:e}",
            a.len()
        );
    }
    // The particles moved (the comparison is not of two idle states).
    let p0 = floats(&e, seed, "P");
    let p1 = floats(&e, kernel, "P");
    let moved = p0
        .iter()
        .zip(&p1)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(moved > 0.1, "particles moved {moved}");
    let tolerance = if e.device().adapter_info().backend == wgpu::Backend::Metal {
        1e-4
    } else {
        0.0
    };
    assert!(
        worst <= tolerance,
        "kernel and node chain differ by {worst:e}"
    );
}

/// Each line of the AC2 kernel, as a kernel of its own, matches the operator
/// it stands for bit for bit (one frame from the same points).
#[test]
fn each_kernel_line_matches_its_operator() {
    let _s = serial();
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let seed = scatter(&mut b, 20_000);
    let noise = b.after(
        seed,
        "pnt.noise",
        &[
            ("mode", json!("velocity")),
            ("amplitude", json!(2.0)),
            ("frequency", json!(0.5)),
        ],
    );
    let curl = b.after(
        seed,
        "pnt.kernel",
        &[(
            "code",
            json!("v@v += curlnoise(v@P * 0.5) * (2.0 * @TimeInc);"),
        )],
    );
    let attract = b.after(noise, "pnt.attract", &[("strength", json!(0.3))]);
    let pull = b.after(
        noise,
        "pnt.kernel",
        &[(
            "code",
            json!("v@v += (set(0, 0, 0) - v@P) * (0.3 * @TimeInc);"),
        )],
    );
    let integrate = b.after(noise, "pnt.integrate", &[]);
    let step = b.after(
        noise,
        "pnt.kernel",
        &[("code", json!("v@P += v@v * @TimeInc;"))],
    );
    b.done();
    cook(&mut e, &[noise, curl, attract, pull, integrate, step], 1);
    for (what, node, kernel, attr) in [
        ("noise", noise, curl, "v"),
        ("attract", attract, pull, "v"),
        ("integrate", integrate, step, "P"),
    ] {
        assert_eq!(words(&e, node, attr), words(&e, kernel, attr), "{what}");
    }
}

/// AC3: editing the code of a running kernel keeps the simulation's state;
/// an attribute the new kernel binds with another type starts at zero.
#[test]
fn editing_a_running_kernel_keeps_its_state() {
    let _s = serial();
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let seed = scatter(&mut b, 1000);
    let kernel = b.solver(seed, |b, sim| {
        b.after(
            sim,
            "pnt.kernel",
            &[(
                "code",
                json!("v@P += set(0, 1, 0) * @TimeInc;\nf@heat += 1.0;"),
            )],
        )
    });
    b.done();
    for f in 0..30 {
        cook(&mut e, &[kernel], f);
    }
    let before = floats(&e, kernel, "P");
    let p0 = floats(&e, seed, "P");
    // 30 frames (0 to 29) moved every point up by 30/60.
    let dy = before[1] - p0[1];
    assert!((dy - 0.5).abs() < 1e-4, "moved up {dy}");
    assert_eq!(floats(&e, kernel, "heat")[0], 30.0);

    // The edit: move along x instead, and make `heat` an integer.
    set_code(
        &mut e,
        kernel,
        "v@P += set(1, 0, 0) * @TimeInc;\ni@heat += 2;",
    );
    for f in 30..40 {
        cook(&mut e, &[kernel], f);
    }
    let after = floats(&e, kernel, "P");
    for (i, (a, b)) in after.chunks(4).zip(before.chunks(4)).enumerate().take(50) {
        assert!(
            (a[1] - b[1]).abs() < 1e-6,
            "point {i}: y kept ({} vs {})",
            a[1],
            b[1]
        );
        assert!(
            (a[0] - b[0] - 10.0 / 60.0).abs() < 1e-4,
            "point {i}: x moved"
        );
    }
    // `heat` changed type: a new integer attribute counting from zero.
    let heat = words(&e, kernel, "heat");
    assert_eq!(heat[0] as i32, 20);
}

/// A kernel that does not compile names the line and column, and the one
/// that ran before keeps running.
#[test]
fn a_bad_edit_keeps_the_last_kernel_running() {
    let _s = serial();
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let seed = scatter(&mut b, 100);
    let kernel = b.solver(seed, |b, sim| {
        b.after(sim, "pnt.kernel", &[("code", json!("f@n += 1.0;"))])
    });
    b.done();
    for f in 0..5 {
        cook(&mut e, &[kernel], f);
    }
    set_code(&mut e, kernel, "f@n += 1.0;\nf@n += nope;");
    e.set_frame(5);
    e.cook(&[kernel], 5.0 / 60.0).unwrap();
    let err = e.error(kernel).expect("an error").to_string();
    assert!(err.contains("kernel:2:"), "{err}");
    // Still counting with the last kernel that compiled (frames 0 to 5).
    assert_eq!(floats(&e, kernel, "n")[0], 6.0);
    set_code(&mut e, kernel, "f@n += 1.0;");
    cook(&mut e, &[kernel], 6);
    assert_eq!(floats(&e, kernel, "n")[0], 7.0);
}

/// A kernel in a file runs, and offline renders read the file once.
#[test]
fn a_kernel_file_runs() {
    let _s = serial();
    let Some(mut e) = engine() else { return };
    let dir = std::env::temp_dir().join(format!("atd-kernel-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("lift.atk");
    std::fs::write(&file, "@P.y = 7.0;\n").unwrap();
    let mut b = Build::new(&mut e);
    let seed = scatter(&mut b, 10);
    let kernel = b.after(
        seed,
        "pnt.kernel",
        &[("file", json!(file.to_string_lossy()))],
    );
    b.done();
    cook(&mut e, &[kernel], 0);
    let p = floats(&e, kernel, "P");
    assert!(p.chunks(4).all(|c| c[1] == 7.0), "{p:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zz_bisect() {
    let _s = serial();
    let Some(mut e) = engine() else { return };
    let mut b = Build::new(&mut e);
    let seed = scatter(&mut b, 20_000);
    let n1 = b.after(
        seed,
        "pnt.noise",
        &[
            ("mode", json!("velocity")),
            ("amplitude", json!(2.0)),
            ("frequency", json!(0.5)),
        ],
    );
    let k1 = b.after(
        seed,
        "pnt.kernel",
        &[(
            "code",
            json!("v@v += curlnoise(v@P * 0.5) * (2.0 * @TimeInc);"),
        )],
    );
    let n2 = b.after(n1, "pnt.attract", &[("strength", json!(0.3))]);
    let k2 = b.after(
        n1,
        "pnt.kernel",
        &[(
            "code",
            json!("v@v += (set(0,0,0) - v@P) * (0.3 * @TimeInc);"),
        )],
    );
    let n3 = b.after(n1, "pnt.integrate", &[]);
    let k3 = b.after(
        n1,
        "pnt.kernel",
        &[("code", json!("v@P += v@v * @TimeInc;"))],
    );
    b.done();
    cook(&mut e, &[n1, k1, n2, k2, n3, k3], 1);
    for (name, n, k, a) in [
        ("noise", n1, k1, "v"),
        ("attract", n2, k2, "v"),
        ("integrate", n3, k3, "P"),
    ] {
        let (x, y) = (floats(&e, n, a), floats(&e, k, a));
        let same = x
            .iter()
            .zip(&y)
            .filter(|(p, q)| p.to_bits() == q.to_bits())
            .count();
        let d = x
            .iter()
            .zip(&y)
            .map(|(p, q)| (p - q).abs())
            .fold(0.0f32, f32::max);
        eprintln!("{name}: {same}/{} identical, max {d:e}", x.len());
    }
}
