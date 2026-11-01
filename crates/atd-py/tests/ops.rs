//! Python custom operators (`docs/python-ops.md`): declarations read without
//! running anything, the trust gate, cooking off the cook thread, errors,
//! timeouts, hot reload with state, offline determinism, round trips.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use atd_core::registry::{OpFamily, PYTHON_CUSTOM, ParamKind, Registry, WireType};
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_py::ops::{NOT_ALLOWED, PythonOps, TrustStore};
use serde_json::json;

/// GPU tests in this binary run one at a time (WARP and several devices).
static GPU: Mutex<()> = Mutex::new(());

/// A project folder in the temp directory with `ops/` files, removed when
/// dropped.
struct TempProject(PathBuf);

impl TempProject {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "atd-pyops-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ops")).unwrap();
        let p = Self(dir);
        for (file, text) in files {
            p.write(file, text);
        }
        p
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.0.join("ops").join(file), text).unwrap();
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

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
        return None;
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// An engine with the built-in IMG and SIG operators and the project's
/// Python operators.
fn engine(ops: &mut PythonOps) -> Option<Engine> {
    let (device, queue) = gpu()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    ops.register(&mut reg, &mut lib);
    let config = EngineConfig {
        width: 16,
        height: 16,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, name: &str) -> NodeId {
    let id = e.alloc_id();
    e.edit(
        "add",
        [Edit::AddNode {
            node: Node::new(id, ty, name),
        }],
    )
    .unwrap();
    id
}

fn set(e: &mut Engine, node: NodeId, param: &str, v: serde_json::Value) {
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

/// Cooks frame `i` offline and returns `node`'s channel `name`.
fn offline_value(e: &mut Engine, node: NodeId, i: u64, name: &str) -> Option<f32> {
    e.set_frame(i);
    e.cook(&[node], i as f64 / 60.0).unwrap();
    e.channels(node).and_then(|c| c.value(name))
}

const COUNTER: &str = r#"import atidarla as atd

@atd.op(
    family="SIG",
    label="Counter",
    summary="Counts up by Step every frame.",
    params=[atd.Float("step", default=1.0, min=0.0, max=10.0, help="Added every frame.")],
    state={"count": 0.0},
)
def counter(ctx):
    ctx.state.count += ctx.par.step
    return {"count": ctx.state.count}


@atd.op(
    family="SIG",
    params=[
        atd.Float("frequency", default=1.0, min=0.0),
        atd.Float("amplitude", default=1.0),
    ],
    inputs=[atd.Input("in")],
)
def lfo(ctx):
    import math
    v = ctx.par.amplitude * math.sin(2 * math.pi * ctx.par.frequency * ctx.time)
    return {"lfo": v + ctx.inputs["in"].get("chan1", 0.0)}
"#;

/// The schema comes from the declaration alone, and is what the parameter
/// panel builds its widgets from (`atd-app/src/params.rs` matches on
/// `ParamKind`): one widget per kind, ranges, labels, help.
#[test]
fn declarations_become_schemas_without_running_the_file() {
    let source = r#"
import atidarla
raise SystemExit("the reader must never run this file")

@atidarla.op(
    family="IMG",
    label="Every Kind",
    params=[
        atidarla.Float("gain", "Gain", default=0.5, min=0.0, max=2.0, help="Scales."),
        atidarla.Int("count", default=3, min=1, max=8),
        atidarla.Toggle("invert", default=True),
        atidarla.Menu("mode", options=("add", "over"), default="over"),
        atidarla.String("caption", default="hi"),
        atidarla.Colour("tint", default=(1.0, 0.5, 0.0)),
        atidarla.Vec("offset", default=(0.25, -0.5)),
    ],
    inputs=[atidarla.Input("mod")],
    size=(8, 4),
    timeout=0.5,
)
def every_kind(ctx):
    return ctx.image()
"#;
    let p = TempProject::new("schema", &[("kinds.py", source)]);
    let mut ops = PythonOps::open(&p.0);
    assert!(ops.errors().is_empty(), "{:?}", ops.errors());
    let decl = ops.ops().next().unwrap().clone();
    assert_eq!(decl.line, 5);
    assert_eq!(decl.size, (8, 4));
    assert_eq!(decl.timeout, Duration::from_millis(500));
    let s = &decl.schema;
    assert_eq!(s.type_id, "py.kinds.every_kind");
    assert_eq!(s.family, OpFamily::Img);
    assert_eq!(s.label, "Every Kind");
    assert!(s.time_dependent && s.capabilities.is_empty());
    assert_eq!(s.outputs[0].wire, WireType::Img);
    assert_eq!(s.inputs[0].name, "mod");
    assert_eq!(s.inputs[0].wire, WireType::Sig);
    let kinds: Vec<&ParamKind> = s.params.iter().map(|p| &p.kind).collect();
    assert_eq!(
        kinds,
        [
            &ParamKind::Float {
                min: Some(0.0),
                max: Some(2.0)
            },
            &ParamKind::Int {
                min: Some(1),
                max: Some(8)
            },
            &ParamKind::Toggle,
            &ParamKind::Menu {
                options: vec!["add".into(), "over".into()]
            },
            &ParamKind::String,
            &ParamKind::Colour,
            &ParamKind::Vec { n: 2 },
        ]
    );
    assert_eq!(s.params[0].help, "Scales.");
    assert_eq!(s.params[1].label, "Count", "labels default to the name");
    assert_eq!(s.params[5].default, json!([1.0, 0.5, 0.0, 1.0]), "alpha 1");
    assert_eq!(s.params[3].default, json!("over"));
    for p in &s.params {
        p.validate(&p.default).unwrap();
    }
    assert_eq!(s.params_json_schema()["properties"]["gain"]["maximum"], 2.0);

    // Registered under its namespaced ID with Python provenance.
    let mut reg = Registry::default();
    let mut lib = OpLibrary::default();
    ops.register(&mut reg, &mut lib);
    assert_eq!(reg.provenance("py.kinds.every_kind"), Some(PYTHON_CUSTOM));
    assert!(lib.contains("py.kinds.every_kind"));
    assert!(!ops.is_allowed());
}

#[test]
fn bad_declarations_report_their_line() {
    let cases: &[(&str, &str, Option<u32>, &str)] = &[
        (
            "computed",
            "import atidarla as atd\nN = 3\n@atd.op(family='SIG', params=[atd.Int('n', default=N)])\ndef f(ctx): pass\n",
            Some(3),
            "literals",
        ),
        (
            "unknown_arg",
            "import atidarla as atd\n\n@atd.op(family='SIG', colour=1)\ndef f(ctx): pass\n",
            Some(3),
            "unknown argument colour=",
        ),
        (
            "range",
            "import atidarla as atd\n@atd.op(family='SIG', params=[\n  atd.Float('x', default=5, max=1)])\ndef f(ctx): pass\n",
            Some(3),
            "outside min..max",
        ),
        (
            "uncalled",
            "import atidarla as atd\n@atd.op\ndef f(ctx): pass\n",
            Some(2),
            "parentheses",
        ),
        (
            "syntax",
            "import atidarla as atd\n\ndef f(ctx)\n    pass\n",
            Some(3),
            "SyntaxError",
        ),
        (
            "family",
            "import atidarla as atd\n@atd.op(family='PNT')\ndef f(ctx): pass\n",
            Some(2),
            "family=",
        ),
        (
            "img_input",
            "import atidarla as atd\n@atd.op(family='IMG', inputs=[atd.Input('a', 'IMG')])\ndef f(ctx): pass\n",
            Some(2),
            "SIG inputs only",
        ),
        (
            "Bad-Name",
            "import atidarla as atd\n@atd.op(family='SIG')\ndef f(ctx): pass\n",
            None,
            "file names",
        ),
    ];
    for (stem, source, line, needle) in cases {
        let p = TempProject::new("bad", &[(&format!("{stem}.py"), source)]);
        let ops = PythonOps::open(&p.0);
        let errors = ops.errors();
        let e = errors.first().unwrap_or_else(|| panic!("{stem}: no error"));
        assert_eq!(e.line, *line, "{stem}: {e}");
        assert!(e.message.contains(needle), "{stem}: {e}");
        assert_eq!(e.file, format!("ops/{stem}.py"));
        assert_eq!(ops.ops().count(), 0, "{stem}");
    }
}

#[test]
fn missing_files_and_operators_are_explained() {
    let p = TempProject::new("explain", &[("counter.py", COUNTER)]);
    let ops = PythonOps::open(&p.0);
    assert_eq!(
        ops.explain("py.nope.counter").unwrap(),
        "ops/nope.py does not exist"
    );
    assert_eq!(
        ops.explain("py.counter.other").unwrap(),
        "ops/counter.py does not define an operator named `other`"
    );
    assert_eq!(ops.explain("sig.lfo"), None);
}

/// Project files store the namespaced type ID and read back to the same
/// network (rule 8).
#[test]
fn python_nodes_round_trip_in_project_files() {
    let p = TempProject::new("roundtrip", &[("counter.py", COUNTER)]);
    let mut ops = PythonOps::open(&p.0);
    let mut reg = Registry::default();
    atd_ops_sig::register(&mut reg);
    ops.register(&mut reg, &mut OpLibrary::default());
    let mut net = Network::default();
    let (c, l, k) = (net.alloc_id(), net.alloc_id(), net.alloc_id());
    for edit in [
        Edit::AddNode {
            node: Node::new(c, "py.counter.counter", "counter1"),
        },
        Edit::AddNode {
            node: Node::new(l, "py.counter.lfo", "lfo1"),
        },
        Edit::AddNode {
            node: Node::new(k, "sig.constant", "knobs"),
        },
        Edit::SetParam {
            node: c,
            param: "step".into(),
            value: Some(Param::constant(json!(2.5))),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(k, "out"), Endpoint::new(l, "in")),
        },
        Edit::SetParam {
            node: l,
            param: "amplitude".into(),
            value: Some(Param::bound(json!(1.0), c, "count")),
        },
    ] {
        net.apply(&reg, edit).unwrap();
    }
    let text = atd_format::network_to_string(&net).unwrap();
    assert!(text.contains(r#"type = "py.counter.counter""#), "{text}");
    let back = atd_format::network_from_str(&text)
        .unwrap()
        .validated(&reg)
        .unwrap();
    assert_eq!(back, net);
    assert_eq!(atd_format::network_to_string(&back).unwrap(), text);
}

/// Opening a project runs none of its `ops/` code; nodes say why they do
/// not cook; allowing runs them.
#[test]
fn nothing_runs_until_the_project_is_allowed() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let p = TempProject::new("gate", &[]);
    let marker = p.0.join("ran.txt");
    let source = format!(
        "import atidarla as atd\nopen({:?}, 'w').write('x')\n\n@atd.op(family='SIG')\ndef probe(ctx):\n    return {{'x': 1}}\n",
        marker.to_string_lossy()
    );
    p.write("probe.py", &source);
    let mut ops = PythonOps::open(&p.0);
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let n = add(&mut e, "py.probe.probe", "probe1");
    for i in 0..3 {
        e.set_frame(i);
        e.cook(&[n], i as f64 / 60.0).unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        e.error(n).unwrap().starts_with(NOT_ALLOWED),
        "{:?}",
        e.error(n)
    );
    assert!(!marker.exists(), "the file ran before it was allowed");

    ops.allow();
    assert_eq!(offline_value(&mut e, n, 3, "x"), Some(1.0));
    assert_eq!(e.error(n), None);
    assert!(marker.exists());
}

/// The trust store remembers the exact `ops/` contents; any change asks again.
#[test]
fn the_trust_store_remembers_contents_not_folders() {
    let p = TempProject::new("trust", &[("counter.py", COUNTER)]);
    let store = TrustStore::at(p.0.join("settings").join("allowed.txt"));
    let ops = PythonOps::open(&p.0);
    assert_eq!(ops.hash().len(), 64);
    assert!(!store.contains(ops.hash()));
    store.remember(ops.hash(), &p.0).unwrap();
    store.remember(ops.hash(), &p.0).unwrap();
    assert!(store.contains(ops.hash()));
    let text = std::fs::read_to_string(store.path()).unwrap();
    assert_eq!(text.matches(ops.hash()).count(), 1, "{text}");
    // Line endings do not change the hash (a checkout on another OS).
    p.write("counter.py", &COUNTER.replace('\n', "\r\n"));
    assert_eq!(PythonOps::open(&p.0).hash(), ops.hash());
    // An edit, or a new file, does.
    p.write("counter.py", &COUNTER.replace("1.0", "2.0"));
    assert!(!store.contains(PythonOps::open(&p.0).hash()));
    p.write("counter.py", COUNTER);
    p.write("extra.py", "x = 1\n");
    assert!(!store.contains(PythonOps::open(&p.0).hash()));
}

/// After Remove app data… the trust store writes nothing more this session.
#[test]
fn the_trust_store_writes_nothing_after_app_data_is_removed() {
    let p = TempProject::new("trust-removed", &[("counter.py", COUNTER)]);
    let settings = p.0.join("settings");
    let store = TrustStore::at(settings.join("allowed.txt"));
    let ops = PythonOps::open(&p.0);
    atd_core::user_data::mark_removed(&settings);
    let err = store.remember(ops.hash(), &p.0).unwrap_err();
    assert!(err.to_string().contains("removed this session"), "{err}");
    assert!(!store.path().exists());
}

/// A SIG counter keeps state across frames; an LFO reads its parameters
/// and a SIG input; parameters can follow channels.
#[test]
fn sig_operators_cook_with_state_parameters_and_inputs() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let p = TempProject::new("sig", &[("counter.py", COUNTER)]);
    let mut ops = PythonOps::open(&p.0);
    ops.allow();
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let c = add(&mut e, "py.counter.counter", "counter1");
    set(&mut e, c, "step", json!(2.0));
    for i in 0..5 {
        assert_eq!(
            offline_value(&mut e, c, i, "count"),
            Some(2.0 * (i + 1) as f32)
        );
    }
    let k = add(&mut e, "sig.constant", "knobs");
    set(&mut e, k, "value1", json!(0.5));
    let l = add(&mut e, "py.counter.lfo", "lfo1");
    e.edit(
        "wire",
        [
            Edit::Connect {
                wire: Wire::new(Endpoint::new(k, "out"), Endpoint::new(l, "in")),
            },
            Edit::SetParam {
                node: l,
                param: "frequency".into(),
                value: Some(Param::constant(json!(0.25))),
            },
            // Amplitude follows the counter (12, 14, ...).
            Edit::SetParam {
                node: l,
                param: "amplitude".into(),
                value: Some(Param::bound(json!(0.0), c, "count")),
            },
        ],
    )
    .unwrap();
    // Frame 60: t = 1 s, sin(2π·0.25·1) = 1; the counter has cooked 6 times.
    for i in 5..60 {
        offline_value(&mut e, l, i, "lfo");
    }
    let v = offline_value(&mut e, l, 60, "lfo").unwrap();
    let count = e.channels(c).unwrap().value("count").unwrap();
    assert!((v - (count + 0.5)).abs() < 1e-4, "{v} vs {count} + 0.5");
    assert_eq!(e.error(l), None);
}

/// Two projects open at once (two renderers, an app and a tool) each have
/// their own Python namespace: loading one does not unload the other.
#[test]
fn two_projects_do_not_share_python_namespaces() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let p = TempProject::new("twins", &[("counter.py", COUNTER)]);
    let mut first = PythonOps::open(&p.0);
    first.allow();
    let Some(mut a) = engine(&mut first) else {
        return;
    };
    a.set_mode(CookMode::Offline { fps: 60.0 });
    let ca = add(&mut a, "py.counter.counter", "counter1");
    assert_eq!(offline_value(&mut a, ca, 0, "count"), Some(1.0));
    let mut second = PythonOps::open(&p.0);
    second.allow();
    let mut b = engine(&mut second).unwrap();
    b.set_mode(CookMode::Offline { fps: 60.0 });
    let cb = add(&mut b, "py.counter.counter", "counter1");
    for i in 1..4 {
        assert_eq!(offline_value(&mut b, cb, i - 1, "count"), Some(i as f32));
        assert_eq!(offline_value(&mut a, ca, i, "count"), Some((i + 1) as f32));
    }
    assert_eq!(a.error(ca), None);
    assert_eq!(b.error(cb), None);
}

/// Live, results arrive a frame (or more) late and a slow operator never
/// slows the cook.
#[test]
fn live_results_arrive_late_without_blocking_the_cook() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let slow = "import atidarla as atd, time\n\n@atd.op(family='SIG', state={'n': 0})\ndef slow(ctx):\n    time.sleep(0.1)\n    ctx.state.n += 1\n    return {'n': ctx.state.n}\n";
    let p = TempProject::new("live", &[("slow.py", slow)]);
    let mut ops = PythonOps::open(&p.0);
    ops.allow();
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    let n = add(&mut e, "py.slow.slow", "slow1");
    let start = Instant::now();
    let mut worst = Duration::ZERO;
    let mut t = 0.0;
    e.cook(&[n], t).unwrap();
    assert_eq!(e.channels(n).map(|c| c.len()), Some(0), "nothing yet");
    while e.channels(n).and_then(|c| c.value("n")).unwrap_or(0.0) < 3.0 {
        t += 1.0 / 60.0;
        let f = Instant::now();
        e.cook(&[n], t).unwrap();
        worst = worst.max(f.elapsed());
        assert!(start.elapsed() < Duration::from_secs(20), "results arrive");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(e.error(n), None);
    eprintln!("slowest live cook with a 100 ms Python operator: {worst:?}");
    assert!(worst < Duration::from_millis(50), "{worst:?}");
}

/// A CPU IMG operator: bytes or floats, uploaded at its declared size.
#[test]
fn img_operators_output_their_pixels() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let source = r#"import atidarla as atd

@atd.op(family="IMG", size=(4, 2), params=[atd.Int("blue", default=128, min=0, max=255)])
def ramp(ctx):
    img = ctx.image()
    for y in range(ctx.height):
        for x in range(ctx.width):
            i = (y * ctx.width + x) * 4
            img[i:i + 4] = bytes((x * 64, y * 255, ctx.par.blue, 255))
    return img

@atd.op(family="IMG", size=(2, 1))
def floats(ctx):
    return [1.0, 0.0, 0.0, 1.0, 0.0, 0.5, 1.0, 1.0]
"#;
    let p = TempProject::new("img", &[("pixels.py", source)]);
    let mut ops = PythonOps::open(&p.0);
    ops.allow();
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let r = add(&mut e, "py.pixels.ramp", "ramp1");
    set(&mut e, r, "blue", json!(7));
    let f = add(&mut e, "py.pixels.floats", "floats1");
    e.cook(&[r, f], 0.0).unwrap();
    assert_eq!(e.error(r), None);
    let tex = e.result(r).unwrap();
    assert_eq!((tex.width(), tex.height()), (4, 2));
    let px = atd_interop::readback::read_rgba8(e.device(), e.queue(), tex).unwrap();
    assert_eq!(&px[..8], &[0, 0, 7, 255, 64, 0, 7, 255]);
    assert_eq!(&px[16..20], &[0, 255, 7, 255]);
    let px =
        atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(f).unwrap()).unwrap();
    assert_eq!(px, [255, 0, 0, 255, 0, 128, 255, 255]);
}

/// Exceptions and bad results show on the node with the file's line.
#[test]
fn errors_show_on_the_node_with_the_line() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let source = "import atidarla as atd\n\n@atd.op(family='SIG')\ndef boom(ctx):\n    x = 1\n    return {'v': x / 0}\n\n@atd.op(family='SIG')\ndef wrong(ctx):\n    return 5\n";
    let p = TempProject::new("errors", &[("bad.py", source)]);
    let mut ops = PythonOps::open(&p.0);
    ops.allow();
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let b = add(&mut e, "py.bad.boom", "boom1");
    let w = add(&mut e, "py.bad.wrong", "wrong1");
    e.cook(&[b, w], 0.0).unwrap();
    let msg = e.error(b).unwrap();
    assert!(msg.contains("ZeroDivisionError"), "{msg}");
    assert!(msg.contains("ops/bad.py, line 6, in boom"), "{msg}");
    let msg = e.error(w).unwrap();
    assert!(msg.contains("returns a dict"), "{msg}");
}

/// A cook past its timeout is interrupted where it is stuck; one that cannot
/// be interrupted gets the worker restarted. Neither blocks for long.
#[test]
fn stuck_operators_time_out_and_the_worker_restarts() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let source = r#"import atidarla as atd, time

@atd.op(family="SIG", timeout=0.2)
def spin(ctx):
    while True:
        pass

@atd.op(family="SIG", timeout=0.2)
def nap(ctx):
    time.sleep(30)
    return {}

@atd.op(family="SIG", state={"n": 0})
def fine(ctx):
    ctx.state.n += 1
    return {"n": ctx.state.n}
"#;
    let p = TempProject::new("stuck", &[("stuck.py", source)]);
    let mut ops = PythonOps::open(&p.0);
    ops.allow();
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let fine = add(&mut e, "py.stuck.fine", "fine1");
    assert_eq!(offline_value(&mut e, fine, 0, "n"), Some(1.0));
    assert_eq!(offline_value(&mut e, fine, 1, "n"), Some(2.0));

    let spin = add(&mut e, "py.stuck.spin", "spin1");
    let start = Instant::now();
    e.cook(&[spin], 0.0).unwrap();
    let took = start.elapsed();
    let msg = e.error(spin).unwrap();
    assert!(msg.contains("TimeoutError"), "{msg}");
    // The line it was stuck on (the loop's).
    assert!(msg.contains("(ops/stuck.py, line "), "{msg}");
    assert!(msg.contains(", in spin)"), "{msg}");
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert_eq!(
        ops.host().unwrap().restarts(),
        0,
        "interrupted, not restarted"
    );
    assert_eq!(offline_value(&mut e, fine, 2, "n"), Some(3.0), "state kept");

    let nap = add(&mut e, "py.stuck.nap", "nap1");
    let start = Instant::now();
    e.cook(&[nap], 0.0).unwrap();
    let took = start.elapsed();
    let msg = e.error(nap).unwrap();
    assert!(msg.contains("restarted"), "{msg}");
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert_eq!(ops.host().unwrap().restarts(), 1);
    // The new worker runs everything again, state afresh.
    assert_eq!(offline_value(&mut e, fine, 3, "n"), Some(1.0));
}

/// FR-K4: editing a running operator keeps its state where the declared
/// state is compatible, and resets what is not.
#[test]
fn hot_reload_keeps_compatible_state() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let p = TempProject::new("reload", &[("counter.py", COUNTER)]);
    let mut ops = PythonOps::open(&p.0);
    ops.allow();
    let Some(mut e) = engine(&mut ops) else {
        return;
    };
    e.set_mode(CookMode::Offline { fps: 60.0 });
    let c = add(&mut e, "py.counter.counter", "counter1");
    for i in 0..5 {
        offline_value(&mut e, c, i, "count");
    }
    assert_eq!(e.channels(c).unwrap().value("count"), Some(5.0));

    // New code and a new parameter; the state declaration is unchanged.
    let edited = COUNTER
        .replace(
            "ctx.state.count += ctx.par.step",
            "ctx.state.count += 10 * ctx.par.step * (not ctx.par.hold)",
        )
        .replace(
            r#"help="Added every frame.")],"#,
            r#"help="Added every frame."), atd.Toggle("hold")],"#,
        );
    p.write("counter.py", &edited);
    let reload = e.extend(|r, l| ops.reload(r, l)).expect("a change");
    assert_eq!(reload.files, ["ops/counter.py"]);
    assert!(reload.errors.is_empty(), "{:?}", reload.errors);
    let schema = e.registry().get("py.counter.counter").unwrap();
    assert!(
        schema.params.iter().any(|p| p.name == "hold"),
        "panel shows it"
    );
    assert_eq!(
        offline_value(&mut e, c, 5, "count"),
        Some(15.0),
        "state kept"
    );
    set(&mut e, c, "hold", json!(true));
    assert_eq!(offline_value(&mut e, c, 6, "count"), Some(15.0));
    set(&mut e, c, "hold", json!(false));

    // A broken edit stops the node with the error and keeps the state.
    p.write(
        "counter.py",
        &edited.replace("def counter(ctx):", "def counter(ctx)"),
    );
    let reload = e.extend(|r, l| ops.reload(r, l)).expect("a change");
    assert!(reload.errors[0].message.contains("SyntaxError"));
    e.set_frame(7);
    e.cook(&[c], 7.0 / 60.0).unwrap();
    assert!(
        e.error(c).unwrap().contains("SyntaxError"),
        "{:?}",
        e.error(c)
    );
    p.write("counter.py", &edited);
    e.extend(|r, l| ops.reload(r, l)).expect("a change");
    assert_eq!(
        offline_value(&mut e, c, 8, "count"),
        Some(25.0),
        "continues"
    );

    // `count` becomes an int: incompatible, it starts again.
    p.write(
        "counter.py",
        &edited.replace(
            r#"state={"count": 0.0}"#,
            r#"state={"count": 0, "extra": 1}"#,
        ),
    );
    e.extend(|r, l| ops.reload(r, l)).expect("a change");
    assert_eq!(offline_value(&mut e, c, 9, "count"), Some(10.0), "reset");
    assert_eq!(e.error(c), None);
}

/// The app's path: a watcher thread notices a saved file and `poll` (which
/// never blocks) applies it.
#[test]
fn the_watcher_picks_up_saved_files() {
    let p = TempProject::new("watch", &[("counter.py", COUNTER)]);
    let mut ops = PythonOps::open(&p.0);
    let mut reg = Registry::default();
    let mut lib = OpLibrary::default();
    ops.register(&mut reg, &mut lib);
    ops.watch().unwrap();
    assert_eq!(ops.poll(&mut reg, &mut lib), None, "nothing saved yet");
    let hash = ops.hash().to_owned();
    // A different length, so the change shows even with coarse file times.
    p.write(
        "counter.py",
        &COUNTER.replace(r#"label="Counter""#, r#"label="Frame Counter""#),
    );
    let start = Instant::now();
    let reload = loop {
        let f = Instant::now();
        let r = ops.poll(&mut reg, &mut lib);
        assert!(
            f.elapsed() < Duration::from_millis(50),
            "poll does not block"
        );
        if let Some(r) = r {
            break r;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the watcher saw it"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(reload.files, ["ops/counter.py"]);
    assert_eq!(
        reg.get("py.counter.counter").unwrap().label,
        "Frame Counter"
    );
    assert_ne!(ops.hash(), hash, "a changed file is a new hash to allow");
    // A new file registers its operators too.
    p.write(
        "extra.py",
        "import atidarla as atd\n\n@atd.op(family='SIG')\ndef one(ctx):\n    return {'one': 1}\n",
    );
    let start = Instant::now();
    while reg.get("py.extra.one").is_none() {
        ops.poll(&mut reg, &mut lib);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the new file registers"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(lib.contains("py.extra.one"));
}

/// A reload that removes a parameter a node has set drops the value (and a
/// wired input's wire) from the patch, so the project still loads.
#[test]
fn reload_cleans_the_patch_after_incompatible_schema_changes() {
    let p = TempProject::new("stale", &[("counter.py", COUNTER)]);
    let mut ops = PythonOps::open(&p.0);
    let mut reg = Registry::default();
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    ops.register(&mut reg, &mut lib);
    let mut net = Network::default();
    let (c, l, k, m) = (
        net.alloc_id(),
        net.alloc_id(),
        net.alloc_id(),
        net.alloc_id(),
    );
    for edit in [
        Edit::AddNode {
            node: Node::new(c, "py.counter.counter", "counter1"),
        },
        Edit::AddNode {
            node: Node::new(l, "py.counter.lfo", "lfo1"),
        },
        Edit::AddNode {
            node: Node::new(k, "sig.constant", "knobs"),
        },
        Edit::AddNode {
            node: Node::new(m, "sig.math", "math1"),
        },
        Edit::SetParam {
            node: l,
            param: "frequency".into(),
            value: Some(Param::constant(json!(3.0))),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(k, "out"), Endpoint::new(l, "in")),
        },
        Edit::Connect {
            wire: Wire::new(Endpoint::new(l, "out"), Endpoint::new(m, "a")),
        },
        Edit::SetParam {
            node: c,
            param: "step".into(),
            value: Some(Param::bound(json!(1.0), l, "lfo")),
        },
    ] {
        net.apply(&reg, edit).unwrap();
    }
    p.write(
        "counter.py",
        &COUNTER
            .replace(r#"atd.Float("frequency", default=1.0, min=0.0),"#, "")
            .replace(r#"inputs=[atd.Input("in")],"#, ""),
    );
    ops.reload(&mut reg, &mut lib).unwrap();
    assert!(net.clone().validated(&reg).is_err(), "stale before");
    let (edits, notes) = atd_py::ops::stale_edits(&net, &reg);
    assert_eq!(notes.len(), 2, "{notes:?}");
    for edit in edits {
        net.apply(&reg, edit).unwrap();
    }
    let net = net.validated(&reg).unwrap();
    let lfo = net.node(l).unwrap();
    assert!(!lfo.params.contains_key("frequency"));
    assert_eq!(net.inputs_of(l).count(), 0, "the wire into `in` is gone");
    assert_eq!(net.inputs_of(m).count(), 1, "the wire out stays");
    let step = &net.node(c).unwrap().params["step"];
    assert_eq!(step.bind.as_ref().map(|b| b.node), Some(l), "rebound");
    assert!(atd_py::ops::stale_edits(&net, &reg).0.is_empty());
}

fn render_twice(dir: &Path) -> Option<[Vec<Vec<u8>>; 2]> {
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut ops = PythonOps::open(dir);
        ops.allow();
        let mut e = engine(&mut ops)?;
        e.set_mode(CookMode::Offline { fps: 60.0 });
        let c = add(&mut e, "py.art.counter", "counter1");
        let img = add(&mut e, "py.art.plasma", "plasma1");
        e.edit(
            "bind",
            [Edit::SetParam {
                node: img,
                param: "phase".into(),
                value: Some(Param::bound(json!(0.0), c, "count")),
            }],
        )
        .unwrap();
        let mut frames = Vec::new();
        for i in 0..12 {
            e.set_frame(i);
            e.cook(&[img], i as f64 / 60.0).unwrap();
            assert_eq!(e.error(img), None);
            frames.push(
                atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(img).unwrap())
                    .unwrap(),
            );
        }
        runs.push(frames);
    }
    let b = runs.pop()?;
    let a = runs.pop()?;
    Some([a, b])
}

/// Offline (FR-H1), a deterministic Python operator gives the same frames
/// on every render: each frame waits for its own result.
#[test]
fn offline_renders_of_python_operators_are_deterministic() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let source = r#"import atidarla as atd, math

@atd.op(family="SIG", state={"count": 0.0})
def counter(ctx):
    ctx.state.count += ctx.dt * 3
    return {"count": ctx.state.count}

@atd.op(family="IMG", size=(16, 8), params=[atd.Float("phase")])
def plasma(ctx):
    img = ctx.image()
    for y in range(ctx.height):
        for x in range(ctx.width):
            v = math.sin(x * 0.7 + ctx.par.phase) + math.cos(y * 0.9 - ctx.time)
            i = (y * ctx.width + x) * 4
            img[i:i + 4] = bytes((int(127.5 + 63 * v), int(255 * ctx.time) % 256, x * 16, 255))
    return img
"#;
    let p = TempProject::new("determinism", &[("art.py", source)]);
    let Some([a, b]) = render_twice(&p.0) else {
        return;
    };
    assert_eq!(a, b, "two renders are byte-identical");
    let distinct: std::collections::BTreeSet<&Vec<u8>> = a.iter().collect();
    assert_eq!(distinct.len(), a.len(), "every frame differs");
}
