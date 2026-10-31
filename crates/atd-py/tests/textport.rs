//! The textport (docs/textport.md): console semantics on its worker, the
//! `atidarla` console API against an engine, and never blocking the app.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Node, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_py::textport::{Event, Stream, Textport};
use pyo3::prelude::*;
use serde_json::json;

/// GPU tests in this binary run one at a time.
static GPU: Mutex<()> = Mutex::new(());

const WAIT: Duration = Duration::from_secs(20);

/// What one entry printed, by stream, and whether the console wants more.
#[derive(Debug, Default)]
struct Reply {
    out: String,
    err: String,
    result: String,
    more: bool,
}

fn collect(events: &[Event], reply: &mut Reply) -> bool {
    let mut done = false;
    for e in events {
        match e {
            Event::Output { stream, text } => match stream {
                Stream::Out => reply.out += text,
                Stream::Err => reply.err += text,
                Stream::Result => reply.result += text,
            },
            Event::Done { more } => {
                reply.more = *more;
                done = true;
            }
            Event::Reset => {}
        }
    }
    done
}

/// Runs `text` and waits for it, calling `frame` meanwhile (the app's
/// frame loop).
fn run_with(tp: &mut Textport, text: &str, mut frame: impl FnMut(&mut Textport)) -> Reply {
    tp.submit(text);
    let mut reply = Reply::default();
    let mut events = Vec::new();
    let start = Instant::now();
    loop {
        frame(tp);
        events.clear();
        tp.poll(&mut events);
        if collect(&events, &mut reply) && !tp.is_busy() {
            return reply;
        }
        assert!(start.elapsed() < WAIT, "`{text}` did not finish: {reply:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn run(tp: &mut Textport, text: &str) -> Reply {
    run_with(tp, text, |_| {})
}

/// Waits for the worker to report a fresh namespace.
fn wait_reset(tp: &mut Textport) {
    let mut events = Vec::new();
    let start = Instant::now();
    while !events.contains(&Event::Reset) {
        assert!(start.elapsed() < WAIT, "no reset");
        tp.poll(&mut events);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn statement_output_is_captured() {
    let mut tp = Textport::start().unwrap();
    let r = run(&mut tp, "print('hello', 42)");
    assert_eq!(r.out, "hello 42\n");
    assert_eq!((r.err.as_str(), r.result.as_str(), r.more), ("", "", false));
    let r = run(&mut tp, "import sys; sys.stderr.write('warn\\n')");
    assert_eq!(r.err, "warn\n");
    // The return value of write() is an expression's value.
    assert_eq!(r.result, "5\n");
}

#[test]
fn expression_shows_its_repr() {
    let mut tp = Textport::start().unwrap();
    assert_eq!(run(&mut tp, "[1, 'a', 2.5]").result, "[1, 'a', 2.5]\n");
    assert_eq!(run(&mut tp, "'text'").result, "'text'\n");
    // None shows nothing; `_` holds the last value.
    assert_eq!(run(&mut tp, "None").result, "");
    assert_eq!(run(&mut tp, "6 * 7").result, "42\n");
    assert_eq!(run(&mut tp, "_ + 1").result, "43\n");
    // A block shows the value of its last expression only.
    let r = run(&mut tp, "a = 2\na\na + 1");
    assert_eq!(r.result, "3\n");
}

#[test]
fn exception_shows_a_traceback_and_the_console_keeps_working() {
    let mut tp = Textport::start().unwrap();
    let r = run(&mut tp, "def f():\n    return 1 / 0\n\nf()");
    assert!(
        r.err.starts_with("Traceback (most recent call last):"),
        "{r:?}"
    );
    assert!(r.err.contains("File \"<textport>\", line 2, in f"), "{r:?}");
    assert!(
        r.err.ends_with("ZeroDivisionError: division by zero\n"),
        "{r:?}"
    );
    // The console's own frames are not shown.
    assert!(!r.err.contains("textport.py"), "{r:?}");
    let r = run(&mut tp, "1 +* 2");
    assert!(r.err.contains("SyntaxError"), "{r:?}");
    assert!(!r.more);
    run(&mut tp, "x = 2");
    assert_eq!(run(&mut tp, "x * 3").result, "6\n");
    let r = run(&mut tp, "raise SystemExit");
    assert!(r.err.contains("does not close the textport"), "{r:?}");
    assert_eq!(run(&mut tp, "x").result, "2\n");
}

#[test]
fn incomplete_input_waits_for_more_lines() {
    let mut tp = Textport::start().unwrap();
    let r = run(&mut tp, "for i in range(3):");
    assert!(r.more && r.out.is_empty(), "{r:?}");
    assert!(tp.wants_more());
    let r = run(&mut tp, "    print(i)");
    assert!(r.more && r.out.is_empty(), "{r:?}");
    // An empty line ends the statement, as at Python's prompt.
    let r = run(&mut tp, "");
    assert_eq!((r.out.as_str(), r.more), ("0\n1\n2\n", false));
    assert!(!tp.wants_more());
    // An open bracket waits too, in a block as well.
    assert!(run(&mut tp, "t = (1,").more);
    assert!(!run(&mut tp, "2)").more);
    assert_eq!(run(&mut tp, "t").result, "(1, 2)\n");
    // A block typed with Shift+Enter runs when Enter is pressed.
    let r = run(&mut tp, "for i in range(2):\n    print(i * 10)");
    assert_eq!((r.out.as_str(), r.more), ("0\n10\n", false));
    assert!(run(&mut tp, "d = {\n  'a': 1,").more);
    assert!(!run(&mut tp, "}").more);
    assert_eq!(run(&mut tp, "d").result, "{'a': 1}\n");
}

#[test]
fn namespace_persists_and_resets_on_project_change() {
    let mut tp = Textport::start().unwrap();
    run(&mut tp, "y = 5\nimport math");
    assert_eq!(run(&mut tp, "y + math.floor(1.5)").result, "6\n");
    // An incomplete statement is dropped with the namespace.
    assert!(run(&mut tp, "if True:").more);
    tp.set_project(Some(PathBuf::from("elsewhere.atidarla/ops")), false);
    assert!(!tp.wants_more());
    wait_reset(&mut tp);
    let r = run(&mut tp, "y");
    assert!(
        r.err.ends_with("NameError: name 'y' is not defined\n"),
        "{r:?}"
    );
    assert_eq!(
        run(&mut tp, "atidarla.version()").result.trim(),
        format!("'{}'", atd_core::VERSION)
    );
}

#[test]
fn a_long_statement_does_not_block_the_frame_loop() {
    let mut tp = Textport::start().unwrap();
    run(&mut tp, "import time");
    let mut frames = 0u32;
    let mut slowest = Duration::ZERO;
    let mut last = Instant::now();
    let submitted = Instant::now();
    let r = run_with(&mut tp, "time.sleep(1); print('woke')", |_| {
        // One "frame" of the app: never waits for Python.
        frames += 1;
        slowest = slowest.max(last.elapsed());
        last = Instant::now();
    });
    assert_eq!(r.out, "woke\n");
    assert!(submitted.elapsed() >= Duration::from_secs(1));
    // About 1 s of frames at a 2 ms sleep each; a frame never waited for
    // the statement.
    assert!(frames > 50, "only {frames} frames while Python slept");
    assert!(
        slowest < Duration::from_millis(250),
        "a frame took {slowest:?}"
    );
}

#[test]
fn submit_returns_at_once() {
    let mut tp = Textport::start().unwrap();
    run(&mut tp, "import time");
    tp.submit("time.sleep(1)");
    let t = Instant::now();
    tp.submit("print('after')");
    let mut events = Vec::new();
    tp.poll(&mut events);
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    assert!(tp.is_busy());
    let mut reply = Reply::default();
    let start = Instant::now();
    while tp.is_busy() {
        assert!(start.elapsed() < WAIT);
        collect(&events, &mut reply);
        events.clear();
        std::thread::sleep(Duration::from_millis(5));
        tp.poll(&mut events);
    }
    collect(&events, &mut reply);
    assert_eq!(reply.out, "after\n");
}

#[test]
fn interrupt_stops_a_running_loop() {
    let mut tp = Textport::start().unwrap();
    tp.submit("n = 0\nwhile True:\n    n += 1");
    // Let it start.
    std::thread::sleep(Duration::from_millis(200));
    tp.interrupt();
    let mut reply = Reply::default();
    let mut events = Vec::new();
    let start = Instant::now();
    while tp.is_busy() {
        assert!(start.elapsed() < WAIT, "the loop did not stop");
        tp.poll(&mut events);
        std::thread::sleep(Duration::from_millis(5));
    }
    collect(&events, &mut reply);
    assert!(reply.err.ends_with("KeyboardInterrupt\n"), "{reply:?}");
    // Still usable, and the interrupt does not hit the next entry.
    assert_eq!(run(&mut tp, "n > 0").result, "True\n");
}

#[test]
fn the_console_api_works_only_in_the_textport() {
    // Starts the interpreter and installs the API.
    let mut tp = Textport::start().unwrap();
    run(&mut tp, "pass");
    Python::attach(|py| {
        let atd = py.import("atidarla").unwrap();
        let err = atd.getattr("nodes").unwrap().call0().unwrap_err();
        assert!(err.to_string().contains("only in the textport"), "{err}");
    });
}

/// A project folder in the temp directory with an `ops/` module that
/// leaves a marker file when imported, removed when dropped.
struct TempProject(PathBuf);

impl TempProject {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "atd-textport-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ops")).unwrap();
        std::fs::write(
            dir.join("ops/secret_op.py"),
            "import pathlib\npathlib.Path(__file__).with_name('ran.txt').write_text('ran')\n",
        )
        .unwrap();
        Self(dir)
    }

    fn ops(&self) -> PathBuf {
        self.0.join("ops")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn project_ops_are_not_imported_until_allowed() {
    let project = TempProject::new("trust");
    let mut tp = Textport::start().unwrap();
    tp.set_project(Some(project.ops()), false);
    wait_reset(&mut tp);
    let path = format!(
        "import sys; sys.path.insert(0, r'{}')",
        project.ops().display()
    );
    run(&mut tp, &path);
    let r = run(&mut tp, "import secret_op");
    assert!(r.err.contains("ImportError"), "{r:?}");
    assert!(r.err.contains("not allowed to run"), "{r:?}");
    assert!(!project.ops().join("ran.txt").exists());
    // Once the user allows the project's operators, it imports.
    // (`sys.path` is the process's, so the folder is still on it.)
    tp.allow_ops();
    wait_reset(&mut tp);
    let r = run(&mut tp, "import secret_op");
    assert_eq!(r.err, "");
    assert!(project.ops().join("ran.txt").exists());
    let r = run(
        &mut tp,
        &format!(
            "import sys; sys.path.remove(r'{}'); del sys.modules['secret_op']",
            project.ops().display()
        ),
    );
    assert_eq!(r.err, "");
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
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 4,
        height: 4,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

/// An engine with `knobs` (SIG Constant, value1 = 0.5) and `noise1`.
fn project() -> Option<Engine> {
    let mut e = engine()?;
    let (knobs, noise) = (e.alloc_id(), e.alloc_id());
    e.edit(
        "build",
        [
            Edit::AddNode {
                node: Node::new(knobs, "sig.constant", "knobs"),
            },
            Edit::AddNode {
                node: Node::new(noise, "img.noise", "noise1"),
            },
            Edit::SetParam {
                node: knobs,
                param: "value1".into(),
                value: Some(Param::constant(json!(0.5))),
            },
        ],
    )
    .unwrap();
    Some(e)
}

#[test]
fn reading_the_project() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = project() else { return };
    let mut tp = Textport::start().unwrap();
    e.cook_frame(None, 2.5).unwrap();
    tp.sync(&mut e, 2.5);
    tp.publish(&e);
    let mut frame = |tp: &mut Textport| {
        tp.sync(&mut e, 2.5);
    };
    assert_eq!(
        run_with(&mut tp, "atidarla.nodes()", &mut frame).result,
        "['knobs', 'noise1']\n"
    );
    assert_eq!(
        run_with(&mut tp, "atidarla.par('knobs', 'value1')", &mut frame).result,
        "0.5\n"
    );
    assert_eq!(
        run_with(&mut tp, "atidarla.optype('noise1')", &mut frame).result,
        "'img.noise'\n"
    );
    // Defaults read too.
    let r = run_with(
        &mut tp,
        "p = atidarla.params('noise1'); p['scale'] == atidarla.par('noise1', 'scale')",
        &mut frame,
    );
    assert_eq!(r.result, "True\n", "{r:?}");
    let r = run_with(&mut tp, "atidarla.time(), atidarla.frame()", &mut frame);
    assert_eq!(r.result, "(2.5, 1)\n");
    let r = run_with(&mut tp, "atidarla.par('knobs', 'nope')", &mut frame);
    assert!(
        r.err
            .ends_with("ValueError: node 'knobs' has no parameter 'nope'\n"),
        "{r:?}"
    );
    let r = run_with(&mut tp, "atidarla.par('ghost', 'value1')", &mut frame);
    assert!(
        r.err.ends_with("ValueError: no node named 'ghost'\n"),
        "{r:?}"
    );
}

#[test]
fn setting_a_parameter_is_one_undoable_edit() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = project() else { return };
    let knobs = e.network().node_by_name("knobs").unwrap().id;
    let value = |e: &Engine| {
        e.network()
            .param_value(e.registry(), knobs, "value1")
            .cloned()
    };
    let mut tp = Textport::start().unwrap();
    tp.publish(&e);
    let mut applied = 0;
    // The set waits for the app's frame, which applies it on its thread.
    let r = run_with(
        &mut tp,
        "atidarla.set_par('knobs', 'value1', 0.25)\natidarla.par('knobs', 'value1')",
        |tp| applied += tp.sync(&mut e, 0.0),
    );
    assert_eq!((r.result.as_str(), r.err.as_str()), ("0.25\n", ""));
    assert_eq!(applied, 1);
    assert_eq!(value(&e), Some(json!(0.25)));
    assert_eq!(e.history().undo_label(), Some("Textport: knobs.value1"));
    assert!(e.undo().unwrap());
    assert_eq!(value(&e), Some(json!(0.5)));
    // Refused edits raise in the console and change nothing.
    let r = run_with(
        &mut tp,
        "atidarla.set_par('knobs', 'value1', 'loud')",
        |tp| applied += tp.sync(&mut e, 0.0),
    );
    assert!(r.err.contains("ValueError"), "{r:?}");
    let r = run_with(
        &mut tp,
        "atidarla.set_par('knobs', 'value1', object())",
        |tp| {
            applied += tp.sync(&mut e, 0.0);
        },
    );
    assert!(
        r.err
            .ends_with("TypeError: object is not a parameter value\n"),
        "{r:?}"
    );
    assert_eq!(applied, 1);
    assert_eq!(value(&e), Some(json!(0.5)));
    assert_eq!(e.history().redo_label(), Some("Textport: knobs.value1"));
}

#[test]
fn the_engine_keeps_cooking_while_a_statement_runs() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = project() else { return };
    let mut tp = Textport::start().unwrap();
    let start = Instant::now();
    let mut slowest = Duration::ZERO;
    let r = run_with(&mut tp, "import time; time.sleep(1); print('done')", |tp| {
        let t = Instant::now();
        e.cook_frame(None, start.elapsed().as_secs_f64()).unwrap();
        tp.sync(&mut e, start.elapsed().as_secs_f64());
        slowest = slowest.max(t.elapsed());
    });
    assert_eq!(r.out, "done\n");
    // Not blocked is the point: a slow shared CI runner cooks fewer frames
    // in the second; ATD_TIMING_STRICT=1 holds the full rate.
    let strict = std::env::var("ATD_TIMING_STRICT").is_ok_and(|v| v == "1");
    let floor = if strict { 50 } else { 10 };
    assert!(e.frame() > floor, "only {} frames cooked", e.frame());
    assert!(
        slowest < Duration::from_millis(250),
        "a frame took {slowest:?}"
    );
}
