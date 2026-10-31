//! The Script DAT: Python `cook(inputs)` on the shared worker, run only
//! while its exact code is allowed, never waiting on the cook (offline it
//! waits for its result).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Dat, DatCtx, DatInputs, DatOp, Params, SigInputs, Table};
use atd_py::script;
use serde_json::json;

/// The allow-list is process-wide: tests that change it take turns.
static GATE: Mutex<()> = Mutex::new(());

struct Rig {
    net: Network,
    registry: Registry,
    node: NodeId,
    dat: HashMap<NodeId, Dat>,
    sources: Vec<Option<NodeId>>,
    out: Dat,
    op: Box<dyn DatOp>,
}

impl Rig {
    fn new(code: &str) -> Self {
        let mut registry = Registry::default();
        atd_ops_dat::register(&mut registry);
        let mut net = Network::default();
        let node = net.alloc_id();
        net.apply(
            &registry,
            Edit::AddNode {
                node: Node::new(node, script::TYPE_ID, "script1"),
            },
        )
        .unwrap();
        let mut rig = Self {
            net,
            registry,
            node,
            dat: HashMap::new(),
            sources: vec![None, None],
            out: Dat::default(),
            op: script::create(),
        };
        rig.code(code);
        rig
    }

    fn code(&mut self, code: &str) {
        self.net
            .apply(
                &self.registry,
                Edit::SetParam {
                    node: self.node,
                    param: "code".into(),
                    value: Some(Param::constant(json!(code))),
                },
            )
            .unwrap();
    }

    /// Allows the code as it is now.
    fn allow(&self) {
        atd_py::allow::allow_network(&self.net, &self.registry);
    }

    /// Sets the code and allows it (the user typed it).
    fn code_allowed(&mut self, code: &str) {
        self.code(code);
        self.allow();
    }

    fn input(&mut self, i: usize, v: Dat) {
        let id = NodeId(100 + i as u64);
        self.dat.insert(id, v);
        self.sources[i] = Some(id);
    }

    fn cook_in(&mut self, mode: CookMode) -> Result<(), String> {
        let sig = HashMap::new();
        let mut ctx = DatCtx {
            inputs: DatInputs::new(&self.sources, &self.dat),
            sig_inputs: SigInputs::new(&[], &sig),
            output: &mut self.out,
            params: Params::new(&self.net, &self.registry, &sig, self.node).with_mode(mode),
            inputs_changed: true,
            time: 0.0,
            dt: 0.0,
            frame: 0,
            cook_again: false,
        };
        self.op.cook(&mut ctx)
    }

    /// Cooks live until `done` holds; returns the last cook's result.
    fn until(&mut self, done: impl Fn(&Self, &Result<(), String>) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let t = Instant::now();
            let r = self.cook_in(CookMode::Live);
            assert!(t.elapsed() < Duration::from_millis(50), "a cook waited");
            if done(self, &r) {
                return r;
            }
            assert!(Instant::now() < deadline, "timed out: {r:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[test]
fn scripts_run_only_once_allowed() {
    let _gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    atd_py::allow::clear();
    let mut rig = Rig::new("def cook(inputs):\n    return 'ran'\n");
    let e = rig.cook_in(CookMode::Live).unwrap_err();
    assert!(e.starts_with(atd_py::ops::NOT_ALLOWED), "{e}");
    assert_eq!(rig.out.as_text(), "");
    // Allowed later: it runs without anything else changing.
    rig.allow();
    rig.until(|r, _| r.out.as_text() == "ran").unwrap();
    // Changed code is new content: blocked until allowed, then it runs.
    rig.code("def cook(inputs):\n    return 'changed'\n");
    let e = rig.until(|_, r| r.is_err()).unwrap_err();
    assert!(e.starts_with(atd_py::ops::NOT_ALLOWED), "{e}");
    assert_eq!(rig.out.as_text(), "ran");
    rig.allow();
    rig.until(|r, _| r.out.as_text() == "changed").unwrap();
    atd_py::allow::clear();
}

#[test]
fn inputs_become_python_values_and_results_dats() {
    let _gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    atd_py::allow::clear();
    // The default code upper-cases a table.
    let mut rig = Rig::new(
        atd_ops_dat::schemas()
            .iter()
            .find(|s| s.type_id == script::TYPE_ID)
            .unwrap()
            .params[0]
            .default
            .as_str()
            .unwrap(),
    );
    rig.allow();
    rig.input(0, Dat::table(Table::from_rows([["a", "b"], ["c", "d"]])));
    rig.until(|r, _| r.out.as_text() == "A\tB\nC\tD").unwrap();
    assert!(!rig.out.is_text());
    // Text in, both inputs, a dict out; a new input value runs it again.
    rig.code_allowed(
        "def cook(inputs):\n    text, other = inputs\n    return {'words': len(text.split()), 'other': other}\n",
    );
    rig.input(0, Dat::text("three small words"));
    rig.until(|r, _| r.out.cell(0, 1) == Some("3")).unwrap();
    assert_eq!(rig.out.as_table().row(1).unwrap(), ["other", ""]);
    rig.input(1, Dat::text("x"));
    rig.until(|r, _| r.out.cell(1, 1) == Some("x")).unwrap();
    // A list of values is one column; None is empty text.
    rig.code_allowed("def cook(inputs):\n    return [1, 2.5, None]\n");
    rig.until(|r, _| r.out.rows() == 3).unwrap();
    assert_eq!(rig.out.as_text(), "1\n2.5\n");
    // Errors name the line; the last good value stays.
    rig.code_allowed("def cook(inputs):\n    x = 1\n    return 1 / 0\n");
    let e = rig.until(|_, r| r.is_err()).unwrap_err();
    assert!(
        e.contains("ZeroDivisionError") && e.contains("line 3"),
        "{e}"
    );
    assert_eq!(rig.out.rows(), 3);
    rig.code_allowed("def cook(inputs)\n    return 1\n");
    let e = rig
        .until(|_, r| r.as_ref().is_err_and(|e| e.contains("SyntaxError")))
        .unwrap_err();
    assert!(e.contains("line 1"), "{e}");
    rig.code_allowed("x = 1\n");
    let e = rig
        .until(|_, r| r.as_ref().is_err_and(|e| e.contains("cook")))
        .unwrap_err();
    assert!(e.contains("no cook(inputs)"), "{e}");
    // Offline the frame waits for the script.
    rig.code_allowed("import time\ndef cook(inputs):\n    time.sleep(0.05)\n    return 'slow'\n");
    rig.cook_in(CookMode::Offline { fps: 30.0 }).unwrap();
    assert_eq!(rig.out.as_text(), "slow");
    atd_py::allow::clear();
}
