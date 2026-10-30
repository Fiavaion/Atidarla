//! A project's kernel operators (`kernels/*.atk`, FR-K1, FR-K4): files
//! register `kernel.<name>` operators whose controls are their `ch()`
//! declarations, the controls drive the GPU, a save updates the schema and
//! keeps the values that still fit, a reload keeps a simulation's state,
//! and project files round-trip. GPU tests skip without a GPU unless
//! `ATD_REQUIRE_GPU` is set, and run one at a time.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use atd_core::registry::{KERNEL_CUSTOM, OpFamily, ParamKind, Registry, WireType};
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::read_blocking;
use atd_ops_pnt::project::{ProjectKernels, stale_edits};
use serde_json::{Value, json};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A project folder in the temp directory with `kernels/` files, removed
/// when dropped.
struct TempProject(PathBuf);

impl TempProject {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "atd-kernels-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("kernels")).unwrap();
        let p = Self(dir);
        for (file, text) in files {
            p.write(file, text);
        }
        p
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.0.join("kernels").join(file), text).unwrap();
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn builtins() -> (Registry, OpLibrary) {
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    atd_ops_img::library(&mut lib);
    (reg, lib)
}

/// An offline engine (fixed step of 1/60 s) with the PNT and IMG operators
/// and the project's kernels.
fn engine(kernels: &ProjectKernels) -> Option<Engine> {
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
    let (mut reg, mut lib) = builtins();
    kernels.register(&mut reg, &mut lib);
    let config = EngineConfig {
        width: 16,
        height: 8,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    e.set_mode(CookMode::Offline { fps: 60.0 });
    Some(e)
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

fn wire(e: &mut Engine, from: NodeId, to: NodeId, socket: &str) {
    e.edit(
        "wire",
        vec![Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
        }],
    )
    .unwrap();
}

fn set(e: &mut Engine, node: NodeId, param: &str, value: Param) {
    e.edit(
        "set",
        vec![Edit::SetParam {
            node,
            param: param.into(),
            value: Some(value),
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

fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<f32> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    let bytes = read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap();
    let n = g.points() as usize * a.ty().stride() as usize;
    bytes[..n]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn pixel(e: &Engine, node: NodeId) -> [u8; 4] {
    let px =
        atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap();
    px[..4].try_into().unwrap()
}

/// Reloads the files and applies the stale-value edits, as the app does
/// after a save.
fn reload(e: &mut Engine, kernels: &mut ProjectKernels) -> Vec<String> {
    let reload = e.extend(|r, l| kernels.reload(r, l)).expect("a change");
    assert!(reload.errors.is_empty(), "{:?}", reload.errors);
    let (edits, notes) = stale_edits(e.network(), e.registry());
    if !edits.is_empty() {
        e.edit("reload", edits).unwrap();
    }
    notes
}

const LIFT: &str = "// Puts every point at one height.
float h = ch(\"height\", 1.0, 0, 10); // Height of every point.
int n = chi(\"count\", 2, 0, 9);
vec3 s = chv(\"shift\", {0, 0, 0});
@P.y = h;
@P.x = s.x;
f@k = float(n);
";

const TINT: &str = "@Cd = vec4(chv(\"rgb\", {0.5, 0.25, 1}), 1);\n";

/// Each file is an operator `kernel.<name>` with its `ch()` declarations
/// as parameters; bad names and duplicates are errors that name the file.
#[test]
fn kernel_files_register_operators() {
    let p = TempProject::new(
        "register",
        &[
            ("lift.atk", LIFT),
            ("tint.pixels.atk", TINT),
            ("lift.pixels.atk", TINT),
            ("Bad-Name.atk", "@P.y = 1;"),
            ("notes.txt", "not a kernel"),
        ],
    );
    let kernels = ProjectKernels::open(&p.0);
    let (mut reg, mut lib) = builtins();
    kernels.register(&mut reg, &mut lib);

    let lift = reg.get("kernel.lift").expect("kernel.lift");
    assert_eq!(reg.provenance("kernel.lift"), Some(KERNEL_CUSTOM));
    assert_eq!(lift.family, OpFamily::Pnt);
    assert_eq!(lift.label, "Lift");
    assert!(lift.summary.starts_with("Puts every point at one height."));
    assert_eq!(lift.inputs.len(), 1);
    assert_eq!(lift.inputs[0].wire, WireType::Geo);
    assert_eq!(lift.outputs[0].wire, WireType::Geo);
    assert!(lift.time_dependent && lift.cook_budget_us > 0);
    let kinds: Vec<(&str, &ParamKind, &Value)> = lift
        .params
        .iter()
        .map(|p| (p.name.as_str(), &p.kind, &p.default))
        .collect();
    assert_eq!(
        kinds,
        [
            (
                "height",
                &ParamKind::Float {
                    min: Some(0.0),
                    max: Some(10.0)
                },
                &json!(1.0)
            ),
            (
                "count",
                &ParamKind::Int {
                    min: Some(0),
                    max: Some(9)
                },
                &json!(2)
            ),
            ("shift", &ParamKind::Vec { n: 3 }, &json!([0.0, 0.0, 0.0])),
        ]
    );
    assert_eq!(lift.params[0].help, "Height of every point.");
    assert!(lift.params[1].help.contains("kernels/lift.atk"));
    assert!(
        lift.params
            .iter()
            .all(|p| p.name != "code" && p.name != "file")
    );
    assert!(lib.contains("kernel.lift"));

    let tint = reg.get("kernel.tint").expect("kernel.tint");
    assert_eq!(tint.family, OpFamily::Img);
    let inputs: Vec<&str> = tint.inputs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(inputs, ["in0", "in1", "in2", "in3"]);
    assert!(
        tint.inputs
            .iter()
            .all(|s| s.wire == WireType::Img && s.optional)
    );
    assert_eq!(tint.params[0].default, json!([0.5, 0.25, 1.0]));
    assert!(lib.contains("kernel.tint"));

    let errors: Vec<String> = kernels.errors().iter().map(|e| e.to_string()).collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].starts_with("kernels/Bad-Name.atk: "),
        "{errors:?}"
    );
    assert!(
        errors[1].contains("kernels/lift.pixels.atk")
            && errors[1].contains("kernels/lift.atk already defines `kernel.lift`"),
        "{errors:?}"
    );
    assert_eq!(
        kernels.explain("kernel.nope").unwrap(),
        "neither kernels/nope.atk nor kernels/nope.pixels.atk exists"
    );
    assert_eq!(kernels.explain("pnt.kernel"), None);

    // A file that does not compile names its line.
    p.write("broken.atk", "float a = 1;\n@P.y = nope;\n");
    let kernels = ProjectKernels::open(&p.0);
    let e = kernels
        .errors()
        .into_iter()
        .find(|e| e.file == "kernels/broken.atk")
        .unwrap()
        .clone();
    assert_eq!(e.line, Some(2), "{e}");
    assert!(kernels.explain("kernel.broken").unwrap().contains("line 2"));
}

/// The node's parameters are the kernel's `ch()` values: constants and
/// expressions, for Points and Pixels kernels.
#[test]
fn editing_a_ch_value_changes_the_gpu_result() {
    let _s = serial();
    let p = TempProject::new("values", &[("lift.atk", LIFT), ("tint.pixels.atk", TINT)]);
    let kernels = ProjectKernels::open(&p.0);
    let Some(mut e) = engine(&kernels) else {
        return;
    };
    let seed = add(&mut e, "pnt.points", &[("count", json!(64))]);
    let lift = add(&mut e, "kernel.lift", &[]);
    wire(&mut e, seed, lift, "in");
    let tint = add(&mut e, "kernel.tint", &[]);
    cook(&mut e, &[lift, tint], 0);
    // The defaults, while the node has no values of its own.
    let at = |e: &Engine, attr: &str, i: usize| floats(e, lift, attr)[i];
    assert_eq!(at(&e, "P", 1), 1.0);
    assert_eq!(at(&e, "k", 0), 2.0);
    assert_eq!(pixel(&e, tint), [128, 64, 255, 255]);

    set(&mut e, lift, "height", Param::constant(json!(3.5)));
    set(&mut e, lift, "count", Param::constant(json!(7)));
    set(
        &mut e,
        lift,
        "shift",
        Param::constant(json!([0.25, 0.0, 0.0])),
    );
    set(&mut e, tint, "rgb", Param::constant(json!([1.0, 0.0, 0.0])));
    cook(&mut e, &[lift, tint], 1);
    assert_eq!(at(&e, "P", 1), 3.5);
    assert_eq!(at(&e, "P", 0), 0.25);
    assert_eq!(at(&e, "k", 0), 7.0);
    assert_eq!(pixel(&e, tint), [255, 0, 0, 255]);

    // An expression drives a control like any other parameter.
    set(
        &mut e,
        lift,
        "height",
        Param {
            value: json!(0.0),
            expr: Some("frame * 2".into()),
            bind: None,
        },
    );
    cook(&mut e, &[lift], 3);
    assert_eq!(at(&e, "P", 1), 6.0);
}

/// Saving the file with a `ch()` added, removed and retyped updates the
/// schema; values whose name and kind still fit are kept, the others
/// dropped with a note.
#[test]
fn saving_the_file_updates_the_schema_and_keeps_matching_values() {
    let v1 = "float a = ch(\"a\", 1.0);\nint b = chi(\"b\", 1);\nvec3 c = chv(\"c\", {0, 0, 0});\n@P.y = a + float(b) + c.x;\n";
    let p = TempProject::new("save", &[("k.atk", v1)]);
    let mut kernels = ProjectKernels::open(&p.0);
    let (mut reg, mut lib) = builtins();
    kernels.register(&mut reg, &mut lib);
    let mut net = Network::default();
    let id = net.alloc_id();
    for edit in [
        Edit::AddNode {
            node: Node::new(id, "kernel.k", "k1"),
        },
        Edit::SetParam {
            node: id,
            param: "a".into(),
            value: Some(Param::constant(json!(2.5))),
        },
        Edit::SetParam {
            node: id,
            param: "b".into(),
            value: Some(Param::constant(json!(3))),
        },
        Edit::SetParam {
            node: id,
            param: "c".into(),
            value: Some(Param::constant(json!([1.0, 2.0, 3.0]))),
        },
    ] {
        net.apply(&reg, edit).unwrap();
    }
    // `a` stays a float, `b` is removed, `c` becomes a vec2 and `d` is new.
    p.write(
        "k.atk",
        "float a = ch(\"a\", 1.0);\nvec2 c = chu(\"c\", {0, 0});\nfloat d = ch(\"d\", 0.5);\n@P.y = a + c.x + d;\n",
    );
    let reload = kernels.reload(&mut reg, &mut lib).expect("a change");
    assert_eq!(reload.files, ["kernels/k.atk"]);
    let names: Vec<&str> = reg
        .get("kernel.k")
        .unwrap()
        .params
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["a", "c", "d"]);
    let (edits, notes) = stale_edits(&net, &reg);
    assert_eq!(
        notes,
        [
            "k1: dropped the value of `b`",
            "k1: dropped the value of `c`"
        ]
    );
    for edit in edits {
        net.apply(&reg, edit).unwrap();
    }
    let node = net.node(id).unwrap();
    assert_eq!(node.params["a"].value, json!(2.5));
    assert!(!node.params.contains_key("b") && !node.params.contains_key("c"));
    assert!(net.clone().validated(&reg).is_ok());

    // A file that no longer compiles keeps the last schema and reports.
    p.write("k.atk", "float a = ch(\"a\", 1.0)\n");
    let reload = kernels.reload(&mut reg, &mut lib).expect("a change");
    assert_eq!(reload.errors.len(), 1);
    assert_eq!(reg.get("kernel.k").unwrap().params.len(), 3);
    // Saving the same text again changes nothing.
    assert_eq!(kernels.reload(&mut reg, &mut lib), None);
}

/// AC3 for project kernels: saving the file of a running simulation keeps
/// its state and the values of its controls.
#[test]
fn saving_a_running_kernel_keeps_its_state() {
    let _s = serial();
    let up = "float speed = ch(\"speed\", 1.0);\nv@P += set(0, 1, 0) * @TimeInc * speed;\n";
    let p = TempProject::new("state", &[("move.atk", up)]);
    let mut kernels = ProjectKernels::open(&p.0);
    let Some(mut e) = engine(&kernels) else {
        return;
    };
    let seed = add(&mut e, "pnt.points", &[("count", json!(256))]);
    let sim = add(&mut e, "pnt.solver", &[]);
    wire(&mut e, seed, sim, "init");
    let k = add(&mut e, "kernel.move", &[("speed", json!(2.0))]);
    wire(&mut e, sim, k, "in");
    wire(&mut e, k, sim, "prev");
    for f in 0..30 {
        cook(&mut e, &[k], f);
    }
    let p0 = floats(&e, seed, "P");
    let before = floats(&e, k, "P");
    assert!(
        (before[1] - p0[1] - 1.0).abs() < 1e-4,
        "30 frames at 2/s up"
    );

    p.write(
        "move.atk",
        "float speed = ch(\"speed\", 1.0);\nfloat lift = ch(\"lift\", 0.0);\nv@P += set(1, lift, 0) * @TimeInc * speed;\n",
    );
    let notes = reload(&mut e, &mut kernels);
    assert!(notes.is_empty(), "{notes:?}");
    assert!(
        e.registry()
            .get("kernel.move")
            .unwrap()
            .params
            .iter()
            .any(|p| p.name == "lift")
    );
    for f in 30..40 {
        cook(&mut e, &[k], f);
    }
    let after = floats(&e, k, "P");
    for (i, (a, b)) in after.chunks(4).zip(before.chunks(4)).enumerate().take(32) {
        assert!((a[1] - b[1]).abs() < 1e-5, "point {i}: y kept");
        // 10 frames along x at the kept speed of 2.
        assert!(
            (a[0] - b[0] - 20.0 / 60.0).abs() < 1e-4,
            "point {i}: x moved"
        );
    }
}

/// Project files store `kernel.<name>` and the control values, and read
/// back to the same network (rule 8).
#[test]
fn kernel_nodes_round_trip_in_project_files() {
    let p = TempProject::new("roundtrip", &[("lift.atk", LIFT)]);
    let kernels = ProjectKernels::open(&p.0);
    let (mut reg, mut lib) = builtins();
    kernels.register(&mut reg, &mut lib);
    let mut net = Network::default();
    let (a, b) = (net.alloc_id(), net.alloc_id());
    for edit in [
        Edit::AddNode {
            node: Node::new(a, "pnt.points", "points1"),
        },
        Edit::AddNode {
            node: Node::new(b, "kernel.lift", "lift1"),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(a, "out"), Endpoint::new(b, "in")),
        },
        Edit::SetParam {
            node: b,
            param: "height".into(),
            value: Some(Param::constant(json!(0.3))),
        },
        Edit::SetParam {
            node: b,
            param: "shift".into(),
            value: Some(Param::constant(json!([1.0, 0.0, -2.5]))),
        },
    ] {
        net.apply(&reg, edit).unwrap();
    }
    let text = atd_format::network_to_string(&net).unwrap();
    assert!(text.contains(r#"type = "kernel.lift""#), "{text}");
    assert!(text.contains("params.height = 0.3"), "{text}");
    let back = atd_format::network_from_str(&text)
        .unwrap()
        .validated(&reg)
        .unwrap();
    assert_eq!(back, net);
    assert_eq!(atd_format::network_to_string(&back).unwrap(), text);
}
