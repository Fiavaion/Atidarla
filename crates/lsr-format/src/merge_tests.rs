//! Diff (FR-F2) and three-way merge (FR-F3) tests.

use super::*;
use crate::diff::{NodeChange, Presence, Section, diff};
use crate::merge::{ConflictKind, Resolution, Slot, Value, merge};
use serde_json::{Value as Json, json};

const BASE: &str = r#"format = 1
next_id = 4

[[node]]
id = 0
name = "lfo1"
type = "sig.lfo"
pos = [0.0, 0.0]

[[node]]
id = 1
name = "noise1"
type = "img.noise"
pos = [200.0, 0.0]
params.scale = 2.0

[[node]]
id = 2
name = "level1"
type = "img.level"
pos = [400.0, 0.0]
inputs.in = { node = 1, output = "out" }
params.brightness = { value = 1.0, bind = { node = 0, channel = "lfo" } }
params.gamma = 1.2

[[comment]]
id = 3
pos = [0.0, 100.0]
size = [200.0, 100.0]
text = "hello"
"#;

fn base() -> Project {
    Project::with_main("show", network_from_str(BASE).unwrap())
}

/// Edits the main network through its JSON form.
fn edit(p: &Project, f: impl FnOnce(&mut Json)) -> Project {
    let mut p = p.clone();
    let mut repr = serde_json::to_value(p.main().unwrap()).unwrap();
    f(&mut repr);
    p.networks
        .insert(MAIN.into(), serde_json::from_value(repr).unwrap());
    p
}

fn node(repr: &mut Json, id: u64) -> &mut Json {
    repr["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|n| n["id"] == id)
        .unwrap()
}

fn set_param(repr: &mut Json, id: u64, name: &str, value: Option<Json>) {
    let params = node(repr, id)
        .as_object_mut()
        .unwrap()
        .entry("params")
        .or_insert(json!({}));
    match value {
        Some(v) => params[name] = v,
        None => {
            params.as_object_mut().unwrap().remove(name);
        }
    }
}

fn remove_node(repr: &mut Json, id: u64) {
    repr["nodes"]
        .as_array_mut()
        .unwrap()
        .retain(|n| n["id"] != id);
    repr["wires"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["from"]["node"] != id && w["to"]["node"] != id);
}

fn add_node(repr: &mut Json, id: u64, name: &str, ty: &str) {
    repr["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": id, "name": name, "type_id": ty, "pos": [0.0, 300.0] }));
    let next = repr["next_id"].as_u64().unwrap().max(id + 1);
    repr["next_id"] = json!(next);
}

fn wire(repr: &mut Json, from: u64, to: u64, socket: &str) {
    let wires = repr["wires"].as_array_mut().unwrap();
    wires.retain(|w| !(w["to"]["node"] == to && w["to"]["socket"] == socket));
    wires.push(json!({
        "from": { "node": from, "socket": "out" },
        "to": { "node": to, "socket": socket },
    }));
}

fn kinds(m: &merge::Merge) -> Vec<ConflictKind> {
    m.conflicts.iter().map(|c| c.kind).collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lsr-merge-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn identical_projects_have_an_empty_diff() {
    let d = diff(&base(), &base());
    assert!(d.is_empty(), "{d}");
    assert_eq!(d.to_string(), "no changes\n");
}

#[test]
fn every_change_kind_is_detected() {
    let a = base();
    let mut b = edit(&a, |r| {
        node(r, 1)["name"] = json!("noise2");
        node(r, 1)["pos"] = json!([210.0, 5.0]);
        node(r, 0)["type_id"] = json!("sig.noise");
        set_param(r, 1, "scale", Some(json!({ "value": 3.0 })));
        set_param(
            r,
            2,
            "gamma",
            Some(json!({ "value": 1.2, "expr": "t * 2" })),
        );
        set_param(r, 2, "brightness", None);
        add_node(r, 4, "blur1", "img.blur");
        wire(r, 4, 2, "in");
        remove_node(r, 1);
        r["comments"][0]["text"] = json!("changed");
    });
    b.name = "show 2".into();
    b.resolution = [1920, 1080];
    b.ai.sovereign = false;
    b.models.insert(
        "sd-turbo".into(),
        ModelPin {
            licence_class: LicenceClass::CommercialSafe,
            sha256: "a".repeat(64),
        },
    );
    b.timeline = Some(TimelineDoc::default());
    let d = diff(&a, &b);
    let keys: Vec<&str> = d.settings.iter().map(|s| s.key).collect();
    assert_eq!(keys, ["name", "resolution"]);
    let net = d.network(MAIN).unwrap();
    let lfo = net.node(lsr_core::graph::NodeId(0)).unwrap();
    assert_eq!(lfo.presence, Presence::Changed);
    assert!(matches!(&lfo.changes[0], NodeChange::Retyped { new, .. } if new == "sig.noise"));
    assert_eq!(
        net.node(lsr_core::graph::NodeId(1)).unwrap().presence,
        Presence::Removed
    );
    let blur = net.node(lsr_core::graph::NodeId(4)).unwrap();
    assert_eq!(blur.presence, Presence::Added);
    let level = net.node(lsr_core::graph::NodeId(2)).unwrap();
    let text: Vec<String> = level.changes.iter().map(ToString::to_string).collect();
    assert_eq!(
        text,
        [
            "param brightness: { value = 1.0, bind = { node = 0, channel = \"lfo\" } } → default",
            "param gamma: 1.2 → { value = 1.2, expr = \"t * 2\" }",
            "input in: n1.out → n4.out",
        ]
    );
    assert_eq!(net.comments.len(), 1);
    let sections: Vec<&Section> = d.sections.iter().map(|s| &s.section).collect();
    assert_eq!(
        sections,
        [
            &Section::Timeline,
            &Section::Ai,
            &Section::Model("sd-turbo".into())
        ]
    );

    // Renamed and moved, on a node that stays.
    let c = edit(&a, |r| {
        node(r, 1)["name"] = json!("noise2");
        node(r, 1)["pos"] = json!([210.0, 5.0]);
    });
    let d = diff(&a, &c);
    let noise = d
        .network(MAIN)
        .unwrap()
        .node(lsr_core::graph::NodeId(1))
        .unwrap();
    assert!(matches!(noise.changes[0], NodeChange::Renamed { .. }));
    assert!(matches!(noise.changes[1], NodeChange::Moved { .. }));
    let report = d.to_string();
    assert!(report.contains("~ n1 noise2 (img.noise)"), "{report}");
    assert!(report.contains("renamed noise1 → noise2"), "{report}");
    // Moving alone is a move-only change.
    let moved = edit(&a, |r| node(r, 2)["pos"] = json!([0.0, 0.0]));
    let d = diff(&a, &moved);
    assert!(d.network(MAIN).unwrap().nodes[0].is_move_only());
    // Mappings section.
    let mut mapped = a.clone();
    mapped.mappings.next_id = 3;
    assert_eq!(diff(&a, &mapped).sections[0].section, Section::Mappings);
}

#[test]
fn disjoint_edits_merge_to_both() {
    let b = base();
    let ours = edit(&b, |r| {
        set_param(r, 1, "scale", Some(json!({ "value": 5.0 })));
        node(r, 2)["name"] = json!("out1");
    });
    let theirs = edit(&b, |r| {
        set_param(r, 2, "gamma", Some(json!({ "value": 2.2 })));
        add_node(r, 4, "blur1", "img.blur");
        wire(r, 1, 4, "in");
        remove_node(r, 0);
        // Removing lfo1 drops the binding with it.
        set_param(r, 2, "brightness", None);
    });
    let m = merge(&b, &ours, &theirs);
    assert!(m.is_clean(), "{:?}", m.conflicts);
    let both = edit(&b, |r| {
        set_param(r, 1, "scale", Some(json!({ "value": 5.0 })));
        node(r, 2)["name"] = json!("out1");
        set_param(r, 2, "gamma", Some(json!({ "value": 2.2 })));
        add_node(r, 4, "blur1", "img.blur");
        wire(r, 1, 4, "in");
        remove_node(r, 0);
        set_param(r, 2, "brightness", None);
    });
    assert_eq!(m.result().unwrap(), both);
    // Merging is symmetric for disjoint edits, and identity on no edits.
    assert_eq!(merge(&b, &theirs, &ours).result().unwrap(), both);
    assert_eq!(merge(&b, &b, &b).result().unwrap(), b);
    assert_eq!(merge(&b, &ours, &b).result().unwrap(), ours);
    assert_eq!(merge(&b, &both, &both).result().unwrap(), both);
}

/// A merge with one conflict of every kind.
fn conflicted() -> (Project, Project, Project) {
    let mut b = base();
    let main = edit(&b, |r| {
        add_node(r, 5, "keep1", "img.level");
        add_node(r, 6, "gone1", "img.level");
    });
    b.networks.insert(MAIN.into(), main.networks[MAIN].clone());
    b.networks
        .insert("fx".into(), network_from_str(BASE).unwrap());
    let mut ours = edit(&b, |r| {
        set_param(r, 1, "scale", Some(json!({ "value": 5.0 }))); // param
        wire(r, 0, 2, "in"); // input
        node(r, 2)["name"] = json!("ours_level"); // name
        node(r, 0)["type_id"] = json!("sig.noise"); // type
        set_param(r, 5, "gamma", Some(json!({ "value": 3.0 }))); // edit-delete
        add_node(r, 7, "mine", "img.blur"); // add-add
        r["comments"][0]["text"] = json!("ours"); // comment
        node(r, 1)["pos"] = json!([1.0, 1.0]); // position: auto
        wire(r, 6, 1, "in"); // dangling: theirs removes gone1
    });
    ours.name = "ours".into(); // project
    let mut theirs = edit(&b, |r| {
        set_param(r, 1, "scale", Some(json!({ "value": 7.0 })));
        // Unwires level1's input, which ours rewired.
        r["wires"]
            .as_array_mut()
            .unwrap()
            .retain(|w| w["to"]["node"] != 2);
        set_param(r, 2, "gamma", Some(json!({ "value": 1.2, "expr": "t" })));
        node(r, 2)["name"] = json!("theirs_level");
        node(r, 0)["type_id"] = json!("sig.beat");
        remove_node(r, 5);
        remove_node(r, 6);
        add_node(r, 7, "yours", "img.noise");
        r["comments"][0]["text"] = json!("theirs");
        node(r, 1)["pos"] = json!([2.0, 2.0]);
    });
    theirs.name = "theirs".into();
    // Network edited on ours, deleted on theirs.
    let fx = ours.networks.get_mut("fx").unwrap();
    let mut repr = serde_json::to_value(&*fx).unwrap();
    node(&mut repr, 1)["name"] = json!("fx_noise");
    *fx = serde_json::from_value(repr).unwrap();
    theirs.networks.remove("fx");
    (b, ours, theirs)
}

#[test]
fn every_conflict_kind_is_produced_and_resolvable() {
    let (b, ours, theirs) = conflicted();
    let mut m = merge(&b, &ours, &theirs);
    let mut found = kinds(&m);
    found.sort();
    found.dedup();
    assert_eq!(found, ConflictKind::ALL, "{:#?}", m.conflicts);
    assert_eq!(m.auto.len(), 1, "position-only conflict auto-resolves");
    assert!(m.auto[0].note.contains("kept ours"));

    // Nothing writes while unresolved.
    let dir = temp_dir("unresolved");
    assert!(matches!(
        merge::save(&dir, &m),
        Err(merge::MergeError::Unresolved(n)) if n == m.conflicts.len()
    ));
    assert!(!dir.exists());

    // The placeholder is ours (with dangling references dropped).
    let preview = m.preview().unwrap();
    assert_eq!(preview.name, "ours");
    assert!(
        preview
            .main()
            .unwrap()
            .inputs_of(lsr_core::graph::NodeId(1))
            .next()
            .is_none(),
        "dangling wire dropped from the placeholder"
    );

    // Resolve each: theirs for everything except the dangling wire (base:
    // unconnected) and the comment (an edited value).
    for i in 0..m.conflicts.len() {
        let c = &m.conflicts[i];
        let r = match c.kind {
            ConflictKind::Dangling => Resolution::Base,
            ConflictKind::Comment => {
                let mut v = c.ours.clone().unwrap();
                if let Value::Json(j) = &mut v {
                    j["text"] = json!("both");
                }
                Resolution::Value(Some(v))
            }
            ConflictKind::EditDelete if matches!(c.slot, Slot::Network(_)) => Resolution::Ours,
            _ => Resolution::Theirs,
        };
        m.resolve(i, Some(r));
    }
    assert_eq!(m.unresolved(), 0);
    let result = m.result().unwrap();
    assert_eq!(result.name, "theirs");
    let net = result.main().unwrap();
    let id = lsr_core::graph::NodeId;
    assert_eq!(net.node(id(1)).unwrap().params["scale"].value, json!(7.0));
    assert_eq!(net.node(id(1)).unwrap().pos, [1.0, 1.0], "ours' position");
    assert_eq!(net.node(id(2)).unwrap().name, "theirs_level");
    assert_eq!(net.node(id(0)).unwrap().type_id, "sig.beat");
    assert!(net.node(id(5)).is_none() && net.node(id(6)).is_none());
    assert_eq!(net.node(id(7)).unwrap().name, "yours");
    assert_eq!(net.comments().next().unwrap().text, "both");
    assert!(net.inputs_of(id(2)).next().is_none(), "theirs unwired it");
    assert_eq!(result.networks["fx"].node(id(1)).unwrap().name, "fx_noise");

    // A resolved merge saves canonical files that load back the same.
    let dir = temp_dir("resolved");
    merge::save(&dir, &m).unwrap();
    let loaded = load(&dir).unwrap();
    assert_eq!(loaded, result);
    let first = std::fs::read_to_string(dir.join("networks/main.toml")).unwrap();
    save(&dir, &loaded).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("networks/main.toml")).unwrap(),
        first,
        "canonical"
    );
    assert_eq!(first, network_to_string(net).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn choices_that_do_not_fit_are_refused() {
    let (b, ours, theirs) = conflicted();
    let mut m = merge(&b, &ours, &theirs);
    for i in 0..m.conflicts.len() {
        m.resolve(i, Some(Resolution::Ours));
    }
    // Ours for the dangling wire keeps a wire from gone1, which theirs deleted.
    let err = m.result().unwrap_err().to_string();
    assert!(err.contains("missing node"), "{err}");
}

#[test]
fn two_nodes_with_one_name_conflict() {
    let b = base();
    let ours = edit(&b, |r| add_node(r, 4, "blur1", "img.blur"));
    let theirs = edit(&b, |r| {
        add_node(r, 4, "x", "img.noise");
        add_node(r, 5, "blur1", "img.blur");
    });
    let mut m = merge(&b, &ours, &theirs);
    assert_eq!(kinds(&m), [ConflictKind::AddAdd, ConflictKind::Name]);
    m.resolve(0, Some(Resolution::Ours));
    m.resolve(1, Some(Resolution::Theirs));
    assert!(m.result().unwrap_err().to_string().contains("two nodes"));
    let renamed = merge::parse_value(&m.conflicts[1].slot, "\"blur2\"").unwrap();
    m.resolve(1, Some(Resolution::Value(renamed)));
    let net = m.result().unwrap().networks[MAIN].clone();
    assert_eq!(net.node(lsr_core::graph::NodeId(5)).unwrap().name, "blur2");
}

#[test]
fn values_read_back_from_their_text() {
    let (b, ours, theirs) = conflicted();
    let m = merge(&b, &ours, &theirs);
    for c in &m.conflicts {
        if matches!(c.slot, Slot::Pack(_) | Slot::Model(_)) {
            continue;
        }
        for v in [&c.base, &c.ours, &c.theirs] {
            let text = merge::value_text(&c.slot, v.as_ref());
            let back = merge::parse_value(&c.slot, &text).unwrap();
            assert_eq!(&back, v, "{} {text}", c.slot);
        }
    }
    assert_eq!(
        merge::value_text(
            &Slot::Param {
                net: "main".into(),
                node: 1,
                name: "scale".into()
            },
            Some(&Value::Json(json!({ "value": 5.0 })))
        ),
        "5.0"
    );
}

#[test]
fn network_files_merge_with_a_conflict_block() {
    let b = network_to_string(base().main().unwrap()).unwrap();
    let side = |f: &dyn Fn(&mut Json)| network_to_string(edit(&base(), f).main().unwrap()).unwrap();
    let ours = side(&|r| set_param(r, 1, "scale", Some(json!({ "value": 5.0 }))));
    let theirs = side(&|r| set_param(r, 2, "gamma", Some(json!({ "value": 2.0 }))));
    let clean = merge::merge_network_text(&b, &ours, &theirs).unwrap();
    assert_eq!(clean.conflicts, 0);
    assert!(!merge::has_conflict_block(&clean.text));
    let both = side(&|r| {
        set_param(r, 1, "scale", Some(json!({ "value": 5.0 })));
        set_param(r, 2, "gamma", Some(json!({ "value": 2.0 })));
    });
    assert_eq!(clean.text, both);

    let theirs = side(&|r| set_param(r, 1, "scale", Some(json!({ "value": 9.0 }))));
    let conflicted = merge::merge_network_text(&b, &ours, &theirs).unwrap();
    assert_eq!(conflicted.conflicts, 1);
    assert!(conflicted.text.starts_with(&ours), "placeholder is ours");
    assert!(conflicted.text.contains("# lasair-conflict"));
    // Still a valid network file.
    network_from_str(&conflicted.text).unwrap();

    // A project holding it does not load, but opens as a merge.
    let dir = temp_dir("block");
    save(&dir, &base()).unwrap();
    std::fs::write(dir.join("networks/main.toml"), &conflicted.text).unwrap();
    let err = load(&dir).unwrap_err().to_string();
    assert!(err.contains("unresolved merge conflicts"), "{err}");
    let mut m = merge::open_conflicted(&dir).unwrap().unwrap();
    assert_eq!(m.conflicts.len(), 1);
    let c = &m.conflicts[0];
    assert_eq!(c.kind, ConflictKind::Param);
    assert_eq!(c.label, "main / n1 noise1: param scale");
    assert_eq!(c.theirs, Some(Value::Json(json!({ "value": 9.0 }))));
    m.resolve(0, Some(Resolution::Theirs));
    merge::save(&dir, &m).unwrap();
    let loaded = load(&dir).unwrap();
    assert_eq!(
        loaded
            .main()
            .unwrap()
            .node(lsr_core::graph::NodeId(1))
            .unwrap()
            .params["scale"]
            .value,
        json!(9.0)
    );
    assert!(merge::open_conflicted(&dir).unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);

    // A file new on both sides merges against an empty base.
    let added = merge::merge_network_text("", &ours, &ours).unwrap();
    assert_eq!(added.text, ours);
}

#[test]
fn edits_between_turn_one_network_into_the_other() {
    let mut reg = lsr_core::Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/audio-reactive-feedback.lasair");
    let old = load(&dir).unwrap();
    let new = edit(&old, |r| {
        node(r, 4)["name"] = json!("level1"); // swap two names
        node(r, 8)["name"] = json!("noise1");
        node(r, 6)["type_id"] = json!("img.blur"); // retype
        node(r, 6)["params"] = json!({ "size": { "value": 3.0 } });
        remove_node(r, 3);
        add_node(r, 10, "timer2", "sig.timer");
        set_param(
            r,
            4,
            "z",
            Some(json!({ "value": 0.0, "bind": { "node": 10, "channel": "seconds" } })),
        );
        node(r, 0)["pos"] = json!([10.0, 20.0]);
        set_param(r, 1, "gain", Some(json!({ "value": 2.0 })));
        wire(r, 5, 7, "a");
        r["comments"][0]["text"] = json!("changed");
        r["comments"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "id": 11, "text": "new", "pos": [0.0, 0.0], "size": [100.0, 50.0] }));
        r["next_id"] = json!(12);
    });
    let (a, b) = (old.main().unwrap(), new.main().unwrap());
    b.clone().validated(&reg).unwrap();
    let mut net = a.clone();
    for e in diff::edits_between(a, b) {
        net.apply(&reg, e.clone())
            .unwrap_or_else(|err| panic!("{e:?}: {err}"));
    }
    let (nodes, comments) = diff::diff_networks(&net, b);
    assert!(
        nodes.is_empty() && comments.is_empty(),
        "{nodes:#?} {comments:#?}"
    );
    assert!(diff::edits_between(b, b).is_empty());
}
