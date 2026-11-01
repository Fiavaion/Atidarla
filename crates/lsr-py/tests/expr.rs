//! The two expression tiers agree (PRD A22): every native expression gives
//! the same value in Rust and in Python, from the same inputs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lsr_core::dat::{Dat, Table};
use lsr_core::expr::{self, CellAt, EvalError, Expr, ExprValue, PyJob, PyRef, PythonHost};
use lsr_py::expr::PyExprHost;

fn snapshot() -> Vec<PyRef> {
    vec![
        PyRef {
            name: "noise1".into(),
            exists: true,
            params: vec![
                ("scale".into(), vec![4.0]),
                ("size".into(), vec![0.5, 0.25]),
                ("monochrome".into(), vec![1.0]),
            ],
            channels: vec![],
            dat: None,
        },
        PyRef {
            name: "lfo1".into(),
            exists: true,
            params: vec![],
            channels: vec![("lfo".into(), -0.75), ("x y".into(), 2.0)],
            dat: None,
        },
        PyRef {
            name: "table1".into(),
            exists: true,
            params: vec![],
            channels: vec![],
            dat: Some(Dat::table(Table::from_rows([
                ["name", "speed", "label"],
                ["slow", " 0.25", "calm"],
                ["fast", "4", "wild"],
                ["neg", "-1.5e1", ""],
            ]))),
        },
        PyRef {
            name: "text1".into(),
            exists: true,
            params: vec![],
            channels: vec![],
            dat: Some(Dat::text("10\n20\n")),
        },
    ]
}

/// The native evaluator reading the same snapshot as Python.
struct SnapshotEnv<'a> {
    refs: Vec<Option<&'a PyRef>>,
}

impl expr::Env for SnapshotEnv<'_> {
    fn time(&self) -> f64 {
        12.375
    }
    fn frame(&self) -> f64 {
        743.0
    }
    fn me_digits(&self) -> Option<f64> {
        Some(7.0)
    }
    fn param(&self, node: usize, param: &str, index: Option<usize>) -> Result<f64, EvalError> {
        let r = self.refs[node].ok_or(EvalError::NoNode(node))?;
        let values = &r
            .params
            .iter()
            .find(|(n, _)| &**n == param)
            .ok_or(EvalError::NoParam(node))?
            .1;
        match index {
            Some(i) => values.get(i).copied().ok_or(EvalError::Index(node)),
            None if values.len() == 1 => Ok(values[0]),
            None => Err(EvalError::NoParam(node)),
        }
    }
    fn channel(&self, node: usize, channel: &str) -> Result<f64, EvalError> {
        let r = self.refs[node].ok_or(EvalError::NoNode(node))?;
        r.channels
            .iter()
            .find(|(n, _)| &**n == channel)
            .map(|(_, v)| *v)
            .ok_or(EvalError::NoChannel(node))
    }
    fn cell(
        &self,
        node: usize,
        slot: usize,
        row: CellAt<'_>,
        col: CellAt<'_>,
    ) -> Result<f64, EvalError> {
        let r = self.refs[node].ok_or(EvalError::NoNode(node))?;
        let dat = r.dat.as_ref().ok_or(EvalError::NotDat(node))?;
        expr::cell_number(dat, slot, row, col)
    }
}

const CORPUS: &[&str] = &[
    "1 + 2 * 3 - 4 / 5",
    "7 // 2 + -7 // 2 + 7.5 // 2",
    "-7 % 3 + 7 % -3 + 7.25 % 2 + -0.5 % 1",
    "2 ** 3 ** 2 - 2 ** -1 + (-2) ** 3 - -2 ** 2",
    "1_000 + 1e3 + .5 + 2.5e-1",
    "round(2.5) + round(3.5) + round(-0.5) + round(-2.5)",
    "round(1.2345, 2) + round(2.675, 2) + round(1234.5, -2)",
    "int(-2.7) + int(2.7) + float(3) + bool(0.1) + bool(0)",
    "abs(-3) + min(4, 2, 9) + max(1, 5, 5.0) + pow(2, 10)",
    "clamp(5, 0, 1) + clamp(-5, 0, 1) + lerp(10, 20, 0.25)",
    "(1 < 2 < 3) + (1 < 3 < 2) + (2 == 2.0) + (3 != 3) + (2 >= 2) + (1 <= 0)",
    "(0 or 5) + (3 and 4) + (0 and 4) + (not 0) + (not 2)",
    "(10 if 1 > 2 else 20) + (1 if 0 else 2 if 0 else 3)",
    "True + True * 3 - False",
    "degrees(pi) + radians(180) / pi + tau + e",
    "log(8, 2) + log2(8) + log10(1000) + log(e) + exp(1)",
    "hypot(3, 4) + atan2(0.5, 1) + sqrt(16) + atan(1) + asin(0.5) + acos(0.5)",
    "floor(-1.5) + ceil(-1.5) + trunc(-1.5) + floor(2.5)",
    "sin(time) * cos(frame / 60) + tan(0.3)",
    "time * 2 + frame % 24 + frame // 24",
    "me.digits * 10 + me.digits % 2",
    "op('noise1').par.scale / 2 + op('noise1').par.size[1] + op('noise1').par.monochrome",
    "op('lfo1')['lfo'] * 0.5 + 0.5 + op(\"lfo1\")['x y']",
    "clamp(op('lfo1')['lfo'], -0.5, 0.5) if time > 1 else 0",
    "max(0, sin(time * 3.7)) ** 2 * (frame % 2 == 0)",
    "1 / 0",
    "1 // (time - time)",
    "(frame - 743) % 0",
    "0 ** -1",
    "sqrt(-1)",
    "log(0)",
    "asin(2)",
    "op('nothere').par.x",
    "op('noise1').par.nope",
    "op('lfo1')['nope']",
    "0 and 1 / 0",
    "1 or 1 / 0",
    // DAT cells, by index and by name.
    "op('table1')[1, 1] + op('table1')[2, 'speed'] * 10",
    "op('table1')['neg', 'speed'] + op('table1')['fast', 1]",
    "op('table1')[1 + frame % 2, 'speed']",
    "op('table1')[time // 6, 1]",
    "op('text1')[1, 0] + op('text1')['10', 0]",
    "op('table1')[9, 1]",
    "op('table1')[1, 'nope']",
    "op('table1')['nope', 1]",
    "op('table1')[1, 'label']",
    "op('table1')[3, 'label']",
    "op('table1')[0.5, 1]",
    "op('table1')[-1, 1]",
    "op('text1')[0, 1]",
    "op('lfo1')[0, 0]",
    "op('nothere')[0, 0]",
];

fn python_results(jobs: &[PyJob]) -> Vec<Result<ExprValue, String>> {
    // These are the tests' own expressions (the allow-list itself is
    // `expr_trust.rs`).
    lsr_py::allow::allow(jobs.iter().map(|j| lsr_py::allow::Item::Expr {
        node: j.node,
        param: j.param.to_string(),
        source: j.source.to_string(),
    }));
    let mut host = PyExprHost::start().unwrap();
    host.submit(jobs);
    let mut got: Vec<Option<Result<ExprValue, String>>> = vec![None; jobs.len()];
    let mut buf = Vec::new();
    let start = Instant::now();
    while got.iter().any(Option::is_none) {
        assert!(start.elapsed() < Duration::from_secs(30), "Python answered");
        host.collect(&mut buf);
        for (key, r) in buf.drain(..) {
            got[key as usize] = Some(r);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    got.into_iter().map(Option::unwrap).collect()
}

fn job(key: u64, source: &str) -> PyJob {
    PyJob {
        key,
        source: Arc::from(source),
        time: 12.375,
        frame: 743.0,
        me: Arc::from("level7"),
        refs: snapshot(),
        ..PyJob::default()
    }
}

#[test]
fn native_and_python_tiers_agree() {
    let jobs: Vec<PyJob> = CORPUS
        .iter()
        .enumerate()
        .map(|(i, s)| job(i as u64, s))
        .collect();
    let python = python_results(&jobs);
    let snapshot = snapshot();
    for (source, py) in CORPUS.iter().zip(python) {
        let e = Expr::parse(source).unwrap_or_else(|err| panic!("{source} is native: {err}"));
        let env = SnapshotEnv {
            refs: e
                .refs()
                .iter()
                .map(|n| snapshot.iter().find(|r| &*r.name == n.as_str()))
                .collect(),
        };
        let native = e.eval(&env);
        match (&native, &py) {
            (Ok(a), Ok(ExprValue::Number(b))) => assert!(
                (a - b).abs() <= 1e-9 * a.abs().max(1.0),
                "{source}: native {a}, Python {b}"
            ),
            (Err(_), Err(_)) => {}
            _ => panic!("{source}: native {native:?}, Python {py:?}"),
        }
    }
}

#[test]
fn python_handles_the_rest() {
    let sources = [
        "len('abcd') / 2",
        "sum(range(5))",
        "op('noise1').par.size[0] * 4",
        "[1, 2, 3][me.digits % 3]",
        "me.name.count('l')",
        "1 / 0",
        "nope + 1",
        // Tuples and lists drive vectors component by component (A66).
        "(1 + time, 0.5)",
        "[1, 2, 3, 4, 5]",
    ];
    let jobs: Vec<PyJob> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| job(i as u64, s))
        .collect();
    let got = python_results(&jobs);
    assert_eq!(got[0], Ok(ExprValue::Number(2.0)));
    assert_eq!(got[1], Ok(ExprValue::Number(10.0)));
    assert_eq!(got[2], Ok(ExprValue::Number(2.0)));
    assert_eq!(got[3], Ok(ExprValue::Number(2.0)));
    assert_eq!(got[4], Ok(ExprValue::Number(2.0)));
    assert!(
        got[5].as_ref().unwrap_err().contains("ZeroDivisionError"),
        "{:?}",
        got[5]
    );
    assert!(
        got[6].as_ref().unwrap_err().contains("NameError"),
        "{:?}",
        got[6]
    );
    assert_eq!(got[7], Ok(ExprValue::tuple(&[13.375, 0.5]).unwrap()));
    assert!(
        got[8].as_ref().unwrap_err().contains("gives 5 numbers"),
        "{:?}",
        got[8]
    );
    for i in [0, 1, 3, 4] {
        assert!(
            Expr::parse(sources[i]).is_err(),
            "{} is Python only",
            sources[i]
        );
    }
}
