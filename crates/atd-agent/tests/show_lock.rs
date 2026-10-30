//! M5 AC3: Show Lock blocks graph edits in a scripted red-team test. An
//! agent with every scope but Python tries, during a locked show, each way
//! it has to change the graph or step outside its whitelist; all are
//! refused and the network is unchanged. Whitelisted moves within range go
//! through, at once, one undo step each.

use atd_agent::lock::{Allow, Scope};
use atd_agent::{Agent, NetworkWorkspace, Output, ShowLock, Workspace};
use atd_core::Registry;
use serde_json::json;

fn registry() -> Registry {
    let mut r = Registry::default();
    atd_ops_img::register(&mut r);
    atd_ops_sig::register(&mut r);
    r
}

fn json(o: &Output) -> serde_json::Value {
    match o {
        Output::Json(v) => v.clone(),
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn refused(o: &Output) -> String {
    match o {
        Output::Error(e) => e.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A small show: noise → level, an LFO, and a stage-free patch.
fn show(ws: &mut NetworkWorkspace) {
    let mut setup = Agent::new(
        Scope::defaults()
            .into_iter()
            .chain([Scope::Accept])
            .collect(),
        ShowLock::default(),
    );
    for (t, n) in [
        ("img.noise", "noise1"),
        ("img.level", "level1"),
        ("sig.lfo", "lfo1"),
    ] {
        json(&setup.call(ws, "create_node", &json!({"type": t, "name": n})));
    }
    json(&setup.call(
        ws,
        "connect",
        &json!({"from": "noise1", "to": "level1", "input": "in"}),
    ));
    json(&setup.call(ws, "accept_changes", &json!({"label": "build the show"})));
}

#[test]
fn show_lock_refuses_every_graph_edit() {
    let mut ws = NetworkWorkspace::new(registry());
    show(&mut ws);
    let before = ws.network().clone();
    let lock = ShowLock {
        engaged: true,
        stage: false,
        allow: vec![Allow {
            node: Some("level1".into()),
            param: Some("brightness".into()),
            pack: None,
            min: Some(0.2),
            max: Some(1.0),
        }],
    };
    // Every scope except Python, so only the lock stands in the way.
    let scopes = [Scope::Edit, Scope::Accept, Scope::Stage, Scope::Files]
        .into_iter()
        .collect();
    let mut agent = Agent::new(scopes, lock);

    let attempts = [
        ("create_node", json!({"type": "img.blur"})),
        ("delete_node", json!({"node": "noise1"})),
        (
            "connect",
            json!({"from": "noise1", "to": "level1", "input": "in"}),
        ),
        ("disconnect", json!({"to": "level1", "input": "in"})),
        (
            "create_pack",
            json!({"name": "heist", "nodes": ["noise1", "level1"]}),
        ),
        (
            "set_param",
            json!({"node": "level1", "param": "contrast", "value": 2.0}),
        ),
        (
            "set_param",
            json!({"node": "level1", "param": "brightness", "value": 5.0}),
        ),
        (
            "set_param",
            json!({"node": "level1", "param": "brightness", "value": -1.0}),
        ),
        (
            "set_param",
            json!({"node": "level1", "param": "brightness", "expression": "sin(time)"}),
        ),
        (
            "set_param",
            json!({"node": "level1", "param": "brightness", "bind": {"node": "lfo1", "channel": "lfo"}}),
        ),
        (
            "set_param",
            json!({"node": "n1", "param": "contrast", "value": 2.0}),
        ),
        (
            "write_kernel",
            json!({"name": "evil", "target": "points", "source": "v@P += 1;"}),
        ),
        ("stage_launch", json!({"layer": "level1", "slot": 0})),
        ("stage_crossfader", json!({"mixer": "level1", "value": 1.0})),
        ("accept_changes", json!({})),
        ("python_exec", json!({"code": "import os; os.remove('x')"})),
        ("no_such_tool", json!({})),
    ];
    for (tool, args) in &attempts {
        let why = refused(&agent.call(&mut ws, tool, args));
        println!("refused {tool}: {why}");
        assert_eq!(ws.network(), &before, "{tool} changed the graph");
        assert!(agent.staged().is_empty(), "{tool} staged something");
    }
    // Python is not even offered.
    assert!(agent.tools().iter().all(|t| t.name != "python_exec"));

    // The whitelisted move, in range, applies at once as one undo step.
    let steps = ws.history.len();
    let ok = json(&agent.call(
        &mut ws,
        "set_param",
        &json!({"node": "level1", "param": "brightness", "value": 0.6}),
    ));
    assert_eq!(ok["applied"], true);
    assert_eq!(ws.history.len(), steps + 1);
    let level = ws.network().node_by_name("level1").unwrap();
    assert_eq!(level.params["brightness"].value, json!(0.6));
}

#[test]
fn without_the_lock_changes_are_staged_until_accepted() {
    let mut ws = NetworkWorkspace::new(registry());
    let mut agent = Agent::new(Scope::defaults(), ShowLock::default());
    json(&agent.call(
        &mut ws,
        "create_node",
        &json!({"type": "img.noise", "name": "n"}),
    ));
    json(&agent.call(
        &mut ws,
        "create_node",
        &json!({"type": "img.blur", "params": {"size": 4.0}}),
    ));
    json(&agent.call(
        &mut ws,
        "connect",
        &json!({"from": "n", "to": "blur1", "input": "in"}),
    ));
    // Staged, visible to the agent, not applied.
    assert!(ws.network().is_empty());
    let net = json(&agent.call(&mut ws, "get_network", &json!({})));
    assert_eq!(net["nodes"].as_array().unwrap().len(), 2);
    let review = json(&agent.call(&mut ws, "review_changes", &json!({})));
    assert_eq!(review["staged"].as_array().unwrap().len(), 3);
    // This session may not accept on its own; a person accepts.
    assert!(matches!(
        agent.call(&mut ws, "accept_changes", &json!({})),
        Output::Error(_)
    ));
    let errors = agent.accept(&mut ws, "Assistant: noise and blur").unwrap();
    assert!(errors.is_empty());
    assert_eq!(ws.network().len(), 2);
    assert_eq!(ws.history, ["Assistant: noise and blur"], "one undo step");
    // Wrong wires and unknown parameters are answered, not applied.
    // An unknown parameter is left out and named; the node is still made.
    let made = json(&agent.call(
        &mut ws,
        "create_node",
        &json!({"type": "img.blur", "params": {"radius": 1}}),
    ));
    assert_eq!(made["ignored_params"], json!(["radius"]), "{made}");
    assert_eq!(made["params_it_has"], json!(["size"]), "{made}");
    json(&agent.call(&mut ws, "delete_node", &json!({"node": made["name"]})));
    json(&agent.call(
        &mut ws,
        "create_node",
        &json!({"type": "sig.lfo", "name": "lfo"}),
    ));
    let e = refused(&agent.call(
        &mut ws,
        "connect",
        &json!({"from": "lfo", "to": "blur1", "input": "in"}),
    ));
    println!("type mismatch: {e}");
}
