//! `create_pack` (FR-G1, A40): the agent turns nodes into a Pack through
//! staging; accepting adds the Pack and swaps in its instance as one undo
//! step, and the project saves and reloads text-identically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lsr_agent::lock::Scope;
use lsr_agent::{Agent, NetworkWorkspace, Output, ShowLock, Workspace};
use lsr_core::Registry;
use serde_json::{Value, json};

fn registry() -> Registry {
    let mut r = Registry::default();
    lsr_ops_img::register(&mut r);
    lsr_ops_sig::register(&mut r);
    r
}

fn ok(o: Output) -> Value {
    match o {
        Output::Json(v) => v,
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn err(o: Output) -> String {
    match o {
        Output::Error(e) => e,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn agent() -> Agent {
    Agent::new(
        Scope::defaults()
            .into_iter()
            .chain([Scope::Accept])
            .collect(),
        ShowLock::default(),
    )
}

/// noise1 → level1 → blur1, accepted.
fn patch(ws: &mut NetworkWorkspace, a: &mut Agent) {
    for (t, n) in [
        ("img.noise", "noise1"),
        ("img.level", "level1"),
        ("img.blur", "blur1"),
    ] {
        ok(a.call(ws, "create_node", &json!({"type": t, "name": n})));
    }
    for (from, to) in [("noise1", "level1"), ("level1", "blur1")] {
        ok(a.call(
            ws,
            "connect",
            &json!({"from": from, "to": to, "input": "in"}),
        ));
    }
    ok(a.call(
        ws,
        "set_param",
        &json!({"node": "level1", "param": "brightness", "value": 0.8}),
    ));
    ok(a.call(ws, "accept_changes", &json!({"label": "patch"})));
}

fn files(dir: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let text = std::fs::read_to_string(&p).unwrap();
                out.insert(p.strip_prefix(root).unwrap().to_owned(), text);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn create_pack_stages_accepts_as_one_step_and_round_trips() {
    let mut ws = NetworkWorkspace::new(registry());
    let mut a = agent();
    patch(&mut ws, &mut a);
    let before = ws.network().clone();
    let steps = ws.history.len();

    let made = ok(a.call(
        &mut ws,
        "create_pack",
        &json!({
            "name": "look",
            "nodes": ["level1", "blur1"],
            "promote": [{"node": "level1", "param": "brightness", "name": "glow",
                         "label": "Glow", "help": "How bright.", "min": 0.0, "max": 2.0}],
            "version": "1.2.0",
            "label": "Look",
            "summary": "A level and a blur.",
            "help": "# Look\n\nA level and a blur.\n",
            "instance": "look1"
        }),
    ));
    assert_eq!(made["type"], "cmp.pack_look");
    assert_eq!(made["instance"], "look1");
    assert_eq!(made["params"], json!(["glow"]));
    assert_eq!(made["inputs"], json!(["in"]));
    assert_eq!(made["outputs"], json!(["out"]));
    // Staged: the workspace is untouched, the agent sees the instance and
    // can read and set its promoted parameters before accepting.
    assert_eq!(ws.network(), &before);
    assert_eq!(a.staged_packs().len(), 1);
    let schema = ok(a.call(&mut ws, "get_schema", &json!({"type": "cmp.pack_look"})));
    assert_eq!(schema["params"][0]["name"], "glow");
    ok(a.call(
        &mut ws,
        "set_param",
        &json!({"node": "look1", "param": "glow", "value": 1.5}),
    ));
    let review = ok(a.call(&mut ws, "review_changes", &json!({})));
    assert_eq!(review["staged"].as_array().unwrap().len(), 2, "{review}");

    let accepted = ok(a.call(
        &mut ws,
        "accept_changes",
        &json!({"label": "Make Pack look"}),
    ));
    assert_eq!(accepted["errors"], json!([]));
    assert_eq!(ws.history.len(), steps + 1, "one undo step");
    assert!(a.staged_packs().is_empty());
    let net = ws.network();
    assert_eq!(net.len(), 2);
    assert!(net.node_by_name("level1").is_none());
    let inst = net.node_by_name("look1").expect("the instance");
    assert_eq!(inst.type_id, "cmp.pack_look");
    assert_eq!(inst.params["glow"].value, json!(1.5));
    let noise = net.node_by_name("noise1").unwrap().id;
    assert!(
        net.inputs_of(inst.id)
            .any(|(i, src)| i == "in" && src.node == noise)
    );
    let def = ws.packs.get("look").expect("the Pack is in the project");
    assert_eq!(def.version.to_string(), "1.2.0");
    assert_eq!(def.params[0].max, Some(2.0));
    assert!(!def.locked);

    // The project saves and reloads text-identically.
    let base = std::env::temp_dir().join(format!("lsr-agent-pack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (one, two) = (base.join("one.lasair"), base.join("two.lasair"));
    let mut project = lsr_format::Project::new("packs");
    project
        .networks
        .insert(lsr_format::MAIN.to_owned(), ws.network().clone());
    project.packs = ws.packs.clone();
    lsr_format::save(&one, &project).unwrap();
    let loaded = lsr_format::load(&one).unwrap();
    let reg = loaded.packs.register(&registry()).unwrap();
    let main = loaded.main().unwrap().clone().validated(&reg).unwrap();
    let flat = loaded.packs.flatten(&main, &reg).unwrap();
    assert_eq!(flat.network.len(), 3, "noise + the two nodes inside");
    lsr_format::save(&two, &loaded).unwrap();
    let (f1, f2) = (files(&one), files(&two));
    assert!(f1.keys().any(|p| p.ends_with("packs/look/pack.toml")));
    assert!(f1.keys().any(|p| p.ends_with("packs/look/help.md")));
    assert_eq!(f1, f2);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn create_pack_refusals_change_nothing() {
    let mut ws = NetworkWorkspace::new(registry());
    let mut a = agent();
    patch(&mut ws, &mut a);
    let before = ws.network().clone();
    for (args, why) in [
        (
            json!({"name": "Bad Name", "nodes": ["level1"]}),
            "not a Pack name",
        ),
        (json!({"name": "look", "nodes": ["nope"]}), "no node named"),
        (
            json!({"name": "look", "nodes": ["level1"], "promote": [{"node": "blur1", "param": "size"}]}),
            "not one of `nodes`",
        ),
        (
            json!({"name": "look", "nodes": ["level1"], "promote": [{"node": "level1", "param": "radius"}]}),
            "no parameter",
        ),
        (
            json!({"name": "look", "nodes": ["level1"], "version": "one"}),
            "version",
        ),
        (
            json!({"name": "look", "nodes": ["level1"], "promote": [{"node": "level1", "param": "brightness", "min": -50.0}]}),
            "brightness",
        ),
        (
            json!({"name": "look", "nodes": ["level1"], "instance": "noise1"}),
            "exists",
        ),
    ] {
        let e = err(a.call(&mut ws, "create_pack", &args));
        assert!(e.contains(why), "{args}: {e}");
        assert!(
            a.staged().is_empty() && a.staged_packs().is_empty(),
            "{args}"
        );
    }
    // A name already taken by a staged or accepted Pack is refused.
    ok(a.call(
        &mut ws,
        "create_pack",
        &json!({"name": "look", "nodes": ["blur1"]}),
    ));
    let e = err(a.call(
        &mut ws,
        "create_pack",
        &json!({"name": "look", "nodes": ["level1"]}),
    ));
    assert!(e.contains("exists"), "{e}");
    // Without `edit` the tool is neither offered nor callable.
    let mut read_only = Agent::new(Default::default(), ShowLock::default());
    assert!(read_only.tools().iter().all(|t| t.name != "create_pack"));
    err(read_only.call(
        &mut ws,
        "create_pack",
        &json!({"name": "x", "nodes": ["level1"]}),
    ));
    // Discarding drops the staged Pack too.
    ok(a.call(&mut ws, "discard_changes", &json!({})));
    assert!(a.staged_packs().is_empty());
    assert_eq!(ws.network(), &before);
    assert!(ws.packs.is_empty());
}
