//! Red team for CLAUDE.md rule 9: an agent without `python.exec` cannot
//! write Python into the network. A Python-tier expression (anything
//! outside the native subset, PRD A22) or a Script node's code is refused
//! through every tool that sets parameters (and promoting Script code to a
//! Pack parameter is refused), the network is unchanged, and
//! native expressions still go through. With `python.exec` they are
//! accepted.

use atd_agent::lock::Scope;
use atd_agent::{Agent, NetworkWorkspace, Output, ShowLock, Workspace};
use atd_core::Registry;
use serde_json::json;

fn registry() -> Registry {
    let mut r = Registry::default();
    atd_ops_img::register(&mut r);
    atd_ops_sig::register(&mut r);
    atd_ops_dat::register(&mut r);
    r
}

fn agent(python: bool) -> Agent {
    let mut scopes: std::collections::BTreeSet<Scope> = Scope::defaults()
        .into_iter()
        .chain([Scope::Accept])
        .collect();
    if python {
        scopes.insert(Scope::PythonExec);
    }
    Agent::new(scopes, ShowLock::default())
}

fn ok(o: &Output) {
    assert!(matches!(o, Output::Json(_)), "expected success, got {o:?}");
}

fn refused(o: &Output) -> String {
    match o {
        Output::Error(e) => e.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

const PYTHON: &str = "__import__('os').system('echo pwned') * 0";

#[test]
fn python_needs_the_python_exec_scope() {
    let mut ws = NetworkWorkspace::new(registry());
    let mut a = agent(false);
    ok(&a.call(
        &mut ws,
        "create_node",
        &json!({"type": "img.noise", "name": "noise1"}),
    ));
    ok(&a.call(
        &mut ws,
        "create_node",
        &json!({"type": "dat.script", "name": "script1"}),
    ));
    ok(&a.call(&mut ws, "accept_changes", &json!({"label": "setup"})));
    let before = ws.network().clone();

    // set_param with a Python expression.
    let e = refused(&a.call(
        &mut ws,
        "set_param",
        &json!({"node": "noise1", "param": "scale", "expression": PYTHON}),
    ));
    assert!(
        e.contains("python.exec") && e.contains("native subset"),
        "{e}"
    );
    // A list is Python too, however harmless it looks.
    let e = refused(&a.call(
        &mut ws,
        "set_param",
        &json!({"node": "noise1", "param": "scale", "expression": "[1, 2][0]"}),
    ));
    assert!(e.contains("python.exec"), "{e}");
    // Script code, through set_param and through create_node.
    let e = refused(&a.call(
        &mut ws,
        "set_param",
        &json!({"node": "script1", "param": "code", "value": "def cook(i):\n    return 'x'"}),
    ));
    assert!(e.contains("python.exec"), "{e}");
    let e = refused(&a.call(
        &mut ws,
        "create_node",
        &json!({"type": "dat.script", "name": "script2", "params": {"code": "def cook(i): pass"}}),
    ));
    assert!(e.contains("python.exec"), "{e}");
    // Promoting a Script's code to a Pack parameter would let the instance
    // write Python under another name: refused too.
    let e = refused(&a.call(
        &mut ws,
        "create_pack",
        &json!({"name": "evil", "nodes": ["script1"], "promote": [{"node": "script1", "param": "code"}]}),
    ));
    assert!(e.contains("python.exec"), "{e}");
    // build_network reports it as an error of that parameter.
    let o = a.call(
        &mut ws,
        "build_network",
        &json!({"nodes": [{"type": "img.noise", "name": "noise1", "params": {"scale": PYTHON}}]}),
    );
    assert!(
        format!("{o:?}").contains("python.exec"),
        "build_network refuses it: {o:?}"
    );
    let _ = a.call(&mut ws, "accept_changes", &json!({"label": "attempts"}));
    let python_left = ws
        .network()
        .nodes()
        .flat_map(|n| n.params.values())
        .filter_map(|p| p.expr.as_deref())
        .any(|e| atd_core::expr::Expr::parse(e).is_err());
    assert!(!python_left, "no Python expression got in");
    assert_eq!(
        ws.network()
            .node_by_name("script1")
            .unwrap()
            .params
            .get("code"),
        before.node_by_name("script1").unwrap().params.get("code")
    );
    assert!(ws.network().node_by_name("script2").is_none());

    // Native expressions are not Python: allowed.
    ok(&a.call(
        &mut ws,
        "set_param",
        &json!({"node": "noise1", "param": "scale", "expression": "2 + sin(time)"}),
    ));

    // With python.exec the user granted Python: accepted.
    let mut p = agent(true);
    ok(&p.call(
        &mut ws,
        "set_param",
        &json!({"node": "noise1", "param": "scale", "expression": "[1, 2][0]"}),
    ));
    ok(&p.call(
        &mut ws,
        "set_param",
        &json!({"node": "script1", "param": "code", "value": "def cook(i):\n    return 'x'"}),
    ));
}
