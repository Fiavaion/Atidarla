//! ComfyUI Workflow operators (FR-A10, D9, M4 AC6) against a mock ComfyUI
//! server on 127.0.0.1 (`mock_comfy`): import, parameter derivation, the
//! client's run (both progress forms, errors, upload + view), Sovereign
//! mode, and live nodes in an engine (byte-exact inversion, debounce and one
//! prompt in flight, no server).

mod mock_comfy;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lsr_ai::comfy::{self as api, Client, ComfyError, Job, Upload};
use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::{ParamKind, Registry};
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
use lsr_ops_ai::comfy::{ProjectComfy, import};
use mock_comfy::{Form, MockComfy};
use serde_json::{Value, json};

const W: u32 = 24;
const H: u32 = 12;

/// LoadImage → TestTint → ImageInvert → SaveImage.
fn tint_workflow() -> Value {
    json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": "example.png", "upload": "image"}, "_meta": {"title": "Load Image"}},
        "2": {"class_type": "TestTint", "inputs": {"image": ["1", 0], "seed": 3, "strength": 0.5, "mode": "add",
              "blend": "hard", "note": "first\nsecond", "enabled": true}, "_meta": {"title": "Tint"}},
        "3": {"class_type": "ImageInvert", "inputs": {"image": ["2", 0]}, "_meta": {"title": "Invert Image"}},
        "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "lsr"}, "_meta": {"title": "Save Image"}}
    })
}

/// LoadImage → ImageInvert → SaveImage (the example's workflow).
fn invert_workflow() -> Value {
    json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}, "_meta": {"title": "Load Image"}},
        "2": {"class_type": "ImageInvert", "inputs": {"image": ["1", 0]}, "_meta": {"title": "Invert Image"}},
        "3": {"class_type": "SaveImage", "inputs": {"filename_prefix": "lasair", "images": ["2", 0]}, "_meta": {"title": "Save Image"}}
    })
}

/// A project folder in the temp directory, removed when dropped.
struct Project(PathBuf);

impl Project {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lsr-comfy-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.toml"),
            "format = 1\nname = \"Comfy test\"\n",
        )
        .unwrap();
        Self(dir)
    }

    fn workflow(&self, file: &str, v: &Value) -> PathBuf {
        let path = self.0.join(file);
        std::fs::write(&path, v.to_string()).unwrap();
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn rgba_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(rgba).unwrap();
    out
}

/// A port nothing listens on.
fn dead_server() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr.to_string()
}

#[test]
fn import_writes_the_cached_info_and_derives_parameters() {
    let mock = MockComfy::start();
    let p = Project::new("import");
    let wf = p.workflow("tint.json", &tint_workflow());
    let got = import(&wf, &p.0, &mock.server(), None).unwrap();
    assert_eq!(got.type_id, "comfy.tint");
    assert_eq!(got.files, ["comfy/tint.json", "comfy/tint.info.json"]);
    assert_eq!((got.params, got.images, got.has_output), (7, 1, true));
    // Only the classes used are cached, without the server's file list.
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(p.0.join("comfy/tint.info.json")).unwrap())
            .unwrap();
    let classes: Vec<&String> = info.as_object().unwrap().keys().collect();
    assert_eq!(
        classes,
        ["ImageInvert", "LoadImage", "SaveImage", "TestTint"]
    );
    assert_eq!(
        info["LoadImage"]["input"]["required"]["image"][0],
        json!([])
    );
    // Importing again writes the same bytes.
    let before: Vec<Vec<u8>> = got
        .files
        .iter()
        .map(|f| std::fs::read(p.0.join(f)).unwrap())
        .collect();
    import(&wf, &p.0, &mock.server(), Some("tint")).unwrap();
    for (f, b) in got.files.iter().zip(&before) {
        assert_eq!(&std::fs::read(p.0.join(f)).unwrap(), b, "{f}");
    }
    assert_eq!(
        mock.state.lock().unwrap().requests,
        ["GET /object_info", "GET /object_info"],
        "import only reads object_info"
    );

    // The project registers comfy.tint with parameters from the cached info.
    let comfy = ProjectComfy::open(&p.0);
    assert!(comfy.errors().is_empty(), "{:?}", comfy.errors());
    let schema = comfy.schemas().next().unwrap();
    assert_eq!(schema.type_id, "comfy.tint");
    let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "server",
            "run",
            "auto_run",
            "tint_seed",
            "tint_seed_random",
            "tint_strength",
            "tint_mode",
            "tint_blend",
            "tint_note",
            "tint_enabled",
            "save_image_filename_prefix"
        ]
    );
    let get = |n: &str| schema.params.iter().find(|p| p.name == n).unwrap();
    assert_eq!(
        get("tint_strength").kind,
        ParamKind::Float {
            min: Some(0.0),
            max: Some(1.0)
        }
    );
    assert_eq!(
        get("tint_mode").kind,
        ParamKind::Menu {
            options: vec!["add".into(), "multiply".into()]
        }
    );
    assert_eq!(get("tint_blend").default, json!("hard"));
    assert!(get("tint_note").multiline);
    assert_eq!(get("tint_seed").default, json!(3));
    assert_eq!(schema.inputs.len(), 1);
    assert_eq!(schema.inputs[0].name, "in0");
    // The same files give the same operator.
    assert_eq!(ProjectComfy::open(&p.0).schemas().next(), Some(schema));
    let mut reg = Registry::default();
    comfy.register(&mut reg, &mut OpLibrary::default());
    assert_eq!(
        reg.provenance("comfy.tint"),
        Some(lsr_core::registry::COMFY_CUSTOM)
    );
    // A missing workflow is explained.
    assert!(
        comfy
            .explain("comfy.nope")
            .unwrap()
            .contains("comfy/nope.json")
    );
}

fn tint_job(strength: f64, pixels: &[u8]) -> Job {
    let wf = api::parse_workflow(&tint_workflow()).unwrap();
    Job {
        prompt: api::build_prompt(
            &wf,
            &[
                ("2".into(), "seed".into(), json!(5)),
                ("2".into(), "strength".into(), json!(strength)),
            ],
            &[],
        ),
        uploads: vec![Upload {
            node: "1".into(),
            filename: "t.png".into(),
            bytes: rgba_png(3, 2, pixels),
        }],
        output: Some("4".into()),
    }
}

const PIXELS: [u8; 24] = [
    0, 10, 20, 255, 30, 40, 50, 255, 60, 70, 80, 255, //
    90, 100, 110, 255, 200, 210, 220, 255, 250, 251, 252, 255,
];

#[test]
fn a_run_uploads_follows_both_progress_forms_and_views_the_result() {
    let mock = MockComfy::start();
    let client = Client::new(&mock.server()).unwrap();
    for (form, events) in [
        (Form::Legacy, true),
        (Form::State, true),
        (Form::Legacy, false),
    ] {
        mock.configure(|c| {
            c.form = form;
            c.events = events;
        });
        let mut seen = Vec::new();
        let out = client
            .run(tint_job(0.25, &PIXELS), &AtomicBool::new(false), &mut |p| {
                seen.push(p.to_string())
            })
            .unwrap();
        // The result is the input inverted (RGB; LoadImage drops alpha).
        let img = lsr_io::image::decode(&out.image.unwrap()).unwrap();
        let want: Vec<u8> = PIXELS
            .chunks(4)
            .flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], 255])
            .collect();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.rgba, want, "{form:?}");
        assert_eq!(seen[..2], ["uploading input images", "queued"], "{seen:?}");
        if events {
            assert!(
                seen.iter()
                    .any(|s| s.starts_with("running ImageInvert (node 3) ")),
                "{form:?}: {seen:?}"
            );
        }
        // Parameter values are substituted; links and the upload are set.
        let sent = mock.prompts().pop().unwrap();
        assert_eq!(sent["2"]["inputs"]["seed"], 5);
        assert_eq!(sent["2"]["inputs"]["strength"], 0.25);
        assert_eq!(sent["2"]["inputs"]["note"], "first\nsecond");
        assert_eq!(sent["2"]["inputs"]["image"], json!(["1", 0]));
        assert_eq!(sent["3"]["inputs"]["image"], json!(["2", 0]));
        assert_eq!(sent["1"]["inputs"]["image"], "lasair/t.png");
    }
}

#[test]
fn errors_name_the_node_and_the_server() {
    let mock = MockComfy::start();
    let client = Client::new(&mock.server()).unwrap();
    let no = AtomicBool::new(false);
    // Validation on /prompt.
    let e = client
        .run(tint_job(2.0, &PIXELS), &no, &mut |_| {})
        .unwrap_err();
    assert!(matches!(e, ComfyError::Rejected(_)), "{e}");
    assert_eq!(
        e.to_string(),
        "the server refused the workflow: node 2 (TestTint): Value bigger than max of 1.0: strength"
    );
    // A missing image on the server.
    let mut job = tint_job(0.5, &PIXELS);
    job.uploads.clear();
    job.prompt["1"]["inputs"]["image"] = json!("nope.png");
    let e = client.run(job, &no, &mut |_| {}).unwrap_err();
    assert!(
        e.to_string()
            .contains("node 1 (LoadImage): Invalid image file"),
        "{e}"
    );
    // A node failing while it runs, in either progress form.
    for form in [Form::Legacy, Form::State] {
        mock.configure(|c| {
            c.form = form;
            c.fail_class = Some("ImageInvert".into());
        });
        let e = client
            .run(tint_job(0.5, &PIXELS), &no, &mut |_| {})
            .unwrap_err();
        assert_eq!(e.to_string(), "node 3 (ImageInvert) failed: mock failure");
    }
    // No server.
    let dead = dead_server();
    let e = Client::new(&dead)
        .unwrap()
        .run(tint_job(0.5, &PIXELS), &no, &mut |_| {})
        .unwrap_err();
    assert!(matches!(e, ComfyError::Unreachable { .. }), "{e}");
    assert!(
        e.to_string()
            .starts_with(&format!("server not reachable at {dead}")),
        "{e}"
    );
    // A UI-format workflow is explained at import.
    let p = Project::new("ui");
    let wf = p.workflow(
        "ui.json",
        &json!({"nodes": [], "links": [], "version": 0.4}),
    );
    let e = import(&wf, &p.0, &mock.server(), None).unwrap_err();
    assert!(e.contains("Export (API)"), "{e}");
}

/// Sovereign mode (on by default) refuses an internet ComfyUI server before
/// anything is sent: the client, the import and a node all stop at the
/// egress gate.
#[test]
fn sovereign_mode_refuses_an_internet_server() {
    assert!(
        lsr_ai::egress::sovereign(),
        "Sovereign mode is on by default"
    );
    let server = "203.0.113.9:8188";
    let before = lsr_ai::egress::refused();
    let e = Client::new(server).unwrap_err();
    assert!(
        matches!(
            e,
            ComfyError::Egress {
                sovereign: true,
                ..
            }
        ),
        "{e}"
    );
    assert!(lsr_ai::egress::refused() > before);
    let p = Project::new("sovereign");
    let wf = p.workflow("invert.json", &invert_workflow());
    let e = import(&wf, &p.0, server, None).unwrap_err();
    assert!(e.contains("Sovereign mode blocks 203.0.113.9"), "{e}");
    assert!(!p.0.join("comfy").exists(), "nothing was written");

    // A node: the status says why, the cook is not an error.
    let mock = MockComfy::start();
    import(&wf, &p.0, &mock.server(), None).unwrap();
    let Some(mut e) = engine(&p.0) else { return };
    let node = add(
        &mut e,
        "comfy.invert",
        &[("server", json!(server)), ("auto_run", json!(true))],
    );
    let status = cook_until(&mut e, node, |s| s.contains("Sovereign"));
    assert!(
        status.contains("Sovereign mode blocks 203.0.113.9"),
        "{status}"
    );
    assert_eq!(e.error(node), None);
    assert!(mock.prompts().is_empty());
}

// ------------------------------------------------------------------ engine

fn engine(project: &Path) -> Option<Engine> {
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    let comfy = ProjectComfy::open(project);
    assert!(comfy.errors().is_empty(), "{:?}", comfy.errors());
    comfy.register(&mut reg, &mut lib);
    Some(Engine::new(
        device,
        queue,
        reg,
        lib,
        EngineConfig {
            width: W,
            height: H,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    ))
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name(ty.split('.').nth(1).unwrap());
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

fn set(e: &mut Engine, node: NodeId, p: &str, v: Value) {
    e.edit(
        "set",
        [Edit::SetParam {
            node,
            param: p.into(),
            value: Some(Param::constant(v)),
        }],
    )
    .unwrap();
}

fn wire(e: &mut Engine, from: NodeId, to: NodeId, input: &str) {
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        }],
    )
    .unwrap();
}

/// Cooks at 60 fps until the node's status satisfies `done` (10 s at
/// most); returns it. Every cook must be quick: nothing waits on the
/// server (rule 1).
fn cook_until(e: &mut Engine, node: NodeId, done: impl Fn(&str) -> bool) -> String {
    let start = Instant::now();
    let mut t = 0.0;
    let mut slowest = Duration::ZERO;
    loop {
        let c = Instant::now();
        e.cook(&[node], t).unwrap();
        slowest = slowest.max(c.elapsed());
        t += 1.0 / 60.0;
        let status = e.status(node).unwrap_or("").to_owned();
        if done(&status) {
            assert!(
                slowest < Duration::from_millis(250),
                "a cook took {slowest:?}: the render thread waited"
            );
            return status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out; status: {status}"
        );
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn pixels(e: &Engine, node: NodeId) -> (u32, u32, Vec<u8>) {
    let t = e.result(node).expect("a result");
    let rgba = lsr_interop::readback::read_rgba8(e.device(), e.queue(), t).unwrap();
    (t.width(), t.height(), rgba)
}

#[test]
fn a_live_node_inverts_its_input_and_holds_it() {
    let mock = MockComfy::start();
    mock.configure(|c| c.node_delay = Duration::from_millis(40));
    let p = Project::new("live");
    import(
        &p.workflow("invert.json", &invert_workflow()),
        &p.0,
        &mock.server(),
        None,
    )
    .unwrap();
    let Some(mut e) = engine(&p.0) else { return };
    let ramp = add(
        &mut e,
        "img.ramp",
        &[("colour_b", json!([1.0, 0.5, 0.25, 1.0]))],
    );
    let node = add(
        &mut e,
        "comfy.invert",
        &[("server", json!(mock.server())), ("auto_run", json!(true))],
    );
    wire(&mut e, ramp, node, "in0");
    let status = cook_until(&mut e, node, |s| s.starts_with("done in"));
    assert_eq!(e.error(node), None, "{status}");
    e.cook(&[ramp], 1.0).unwrap();
    let (w, h, input) = pixels(&e, ramp);
    let (ow, oh, output) = pixels(&e, node);
    assert_eq!((ow, oh), (w, h));
    let want: Vec<u8> = input
        .chunks(4)
        .flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], 255])
        .collect();
    assert_eq!(
        output, want,
        "the output is the input inverted, byte for byte"
    );
    assert_eq!(mock.prompts().len(), 1);

    // The server goes away: a press of Run says so and keeps the result.
    set(&mut e, node, "server", json!(dead_server()));
    set(&mut e, node, "auto_run", json!(false));
    set(&mut e, node, "run", json!(true));
    let status = cook_until(&mut e, node, |s| s.starts_with("server not reachable"));
    assert_eq!(e.error(node), None, "{status}");
    assert_eq!(pixels(&e, node).2, want, "the last result is held");
}

#[test]
fn auto_run_waits_for_changes_to_settle_and_runs_one_prompt_at_a_time() {
    let mock = MockComfy::start();
    // Each prompt takes about 150 ms.
    mock.configure(|c| c.node_delay = Duration::from_millis(50));
    let p = Project::new("debounce");
    import(
        &p.workflow("invert.json", &invert_workflow()),
        &p.0,
        &mock.server(),
        None,
    )
    .unwrap();
    let Some(mut e) = engine(&p.0) else { return };
    let ramp = add(&mut e, "img.ramp", &[]);
    let node = add(
        &mut e,
        "comfy.invert",
        &[("server", json!(mock.server())), ("auto_run", json!(true))],
    );
    wire(&mut e, ramp, node, "in0");
    // A parameter changes every frame for 1.5 s.
    let mut t = 0.0;
    let start = Instant::now();
    let mut i = 0;
    while start.elapsed() < Duration::from_millis(1500) {
        set(
            &mut e,
            node,
            "save_image_filename_prefix",
            json!(format!("p{i}")),
        );
        e.cook(&[node], t).unwrap();
        t += 1.0 / 60.0;
        i += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    let last = format!("p{}", i - 1);
    // Then it settles: the last value runs.
    let prompts = || mock.prompts();
    let deadline = Instant::now() + Duration::from_secs(10);
    while prompts()
        .last()
        .is_none_or(|p| p["3"]["inputs"]["filename_prefix"] != json!(last))
        || !e.status(node).unwrap_or("").starts_with("done")
    {
        assert!(Instant::now() < deadline, "the last value never ran");
        e.cook(&[node], t).unwrap();
        t += 1.0 / 60.0;
        std::thread::sleep(Duration::from_millis(16));
    }
    let n = prompts().len();
    assert_eq!(
        mock.state.lock().unwrap().max_running,
        1,
        "one prompt in flight"
    );
    assert!(
        (2..=6).contains(&n),
        "{n} prompts for {i} changes: changes are debounced"
    );
    // Nothing more runs once it is up to date.
    for _ in 0..30 {
        e.cook(&[node], t).unwrap();
        t += 1.0 / 60.0;
        std::thread::sleep(Duration::from_millis(16));
    }
    assert_eq!(prompts().len(), n);
}

#[test]
fn without_auto_run_nothing_runs_until_run_is_pressed() {
    let mock = MockComfy::start();
    let p = Project::new("manual");
    import(
        &p.workflow("invert.json", &invert_workflow()),
        &p.0,
        &mock.server(),
        None,
    )
    .unwrap();
    let Some(mut e) = engine(&p.0) else { return };
    let node = add(&mut e, "comfy.invert", &[("server", json!(mock.server()))]);
    for f in 0..20 {
        e.cook(&[node], f64::from(f) / 60.0).unwrap();
    }
    assert!(mock.prompts().is_empty());
    assert_eq!(e.error(node), None);
    assert!(e.status(node).unwrap().starts_with("not run yet"));
    // Unconnected, LoadImage keeps the workflow's own image.
    set(&mut e, node, "run", json!(true));
    cook_until(&mut e, node, |s| s.starts_with("done in"));
    let sent = mock.prompts();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["1"]["inputs"]["image"], "example.png");
    // A result of another size sizes the output from the next frame on.
    e.cook(&[node], 1.0).unwrap();
    assert_eq!(pixels(&e, node).0, 2, "the result's size");
}
