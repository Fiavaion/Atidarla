//! Learn mode tests: the shipped quests, each check kind, and scripted
//! students who play whole quests. None needs a GPU.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lsr_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use lsr_core::registry::{COMFY_CUSTOM, Registry};
use lsr_format::Mappings;
use lsr_format::mappings::{Source, Target};
use lsr_stage::Stage;
use serde_json::{Value, json};

use super::check::{Check, CookInfo, Snapshot};
use super::progress::{Progress, advance};
use super::quest::{Quest, SHIPPED, check_set, shipped};
use super::*;

type Cooks = BTreeMap<NodeId, (u64, Option<String>)>;

impl CookInfo for Cooks {
    fn cooks(&self, node: NodeId) -> u64 {
        self.get(&node).map_or(0, |c| c.0)
    }

    fn error(&self, node: NodeId) -> Option<&str> {
        self.get(&node)?.1.as_deref()
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A project as a check sees it, with helpers to edit it the way a student
/// does.
struct World {
    registry: Registry,
    net: Network,
    cooks: Cooks,
    mappings: Mappings,
    packs: Vec<String>,
    dir: Option<PathBuf>,
}

impl World {
    fn new() -> Self {
        Self::with(crate::app::builtin_registry(), Network::default())
    }

    fn with(registry: Registry, net: Network) -> Self {
        Self {
            registry,
            net,
            cooks: Cooks::new(),
            mappings: Mappings::default(),
            packs: Vec::new(),
            dir: None,
        }
    }

    fn edit(&mut self, edit: Edit) {
        self.net
            .apply(&self.registry, edit)
            .unwrap_or_else(|e| panic!("edit refused: {e}"));
    }

    fn add(&mut self, ty: &str) -> NodeId {
        let id = self.net.alloc_id();
        let base = ty.rsplit('.').next().unwrap();
        let name = self.net.unique_name(base);
        self.edit(Edit::AddNode {
            node: Node::new(id, ty, name),
        });
        id
    }

    fn wire(&mut self, from: NodeId, to: NodeId, input: &str) {
        self.edit(Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        });
    }

    fn set(&mut self, node: NodeId, name: &str, value: Value) {
        self.edit(Edit::SetParam {
            node,
            param: name.into(),
            value: Some(Param::constant(value)),
        });
    }

    fn bind(&mut self, node: NodeId, name: &str, src: NodeId, channel: &str) {
        let fallback = self
            .net
            .param_value(&self.registry, node, name)
            .cloned()
            .unwrap();
        self.edit(Edit::SetParam {
            node,
            param: name.into(),
            value: Some(Param::bound(fallback, src, channel)),
        });
    }

    fn cooked(&mut self, node: NodeId) {
        self.cooks.insert(node, (3, None));
    }

    fn failed(&mut self, node: NodeId, why: &str) {
        self.cooks.insert(node, (3, Some(why.into())));
    }

    fn by_type(&self, ty: &str) -> NodeId {
        self.net.nodes().find(|n| n.type_id == ty).unwrap().id
    }

    fn eval(&self, check: &Check) -> bool {
        let stage = Stage::read(&self.net, &self.registry);
        let snapshot = Snapshot {
            network: &self.net,
            registry: &self.registry,
            cook: &self.cooks,
            stage: Some(&stage),
            mappings: &self.mappings,
            pack_instances: &self.packs,
            project_dir: self.dir.as_deref(),
        };
        check.eval(&snapshot)
    }

    fn advance(&self, quest: &Quest, done: &mut usize) -> bool {
        let stage = Stage::read(&self.net, &self.registry);
        let snapshot = Snapshot {
            network: &self.net,
            registry: &self.registry,
            cook: &self.cooks,
            stage: Some(&stage),
            mappings: &self.mappings,
            pack_instances: &self.packs,
            project_dir: self.dir.as_deref(),
        };
        advance(quest, done, &snapshot)
    }
}

fn parse(src: &str) -> Check {
    let doc: toml_edit::DocumentMut = format!("c = {src}").parse().unwrap();
    Check::parse(doc["c"].as_table_like().unwrap()).unwrap()
}

fn quest(id: &str) -> Quest {
    shipped().into_iter().find(|q| q.id == id).unwrap()
}

// ---- the shipped quests ----

#[test]
fn shipped_quests_parse_validate_and_match_the_files() {
    let mut files: Vec<String> = std::fs::read_dir(root().join("learn/quests"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".toml"))
        .map(|n| n.trim_end_matches(".toml").to_owned())
        .collect();
    files.sort();
    let listed: Vec<&str> = SHIPPED.iter().map(|(id, _)| *id).collect();
    assert_eq!(files, listed, "learn/quests/ and quest::SHIPPED differ");
    let quests: Vec<Quest> = SHIPPED
        .iter()
        .map(|(id, src)| Quest::parse(id, src).unwrap_or_else(|e| panic!("{e}")))
        .collect();
    assert!(quests.len() >= 10);
    assert_eq!(check_set(&quests), Vec::<String>::new());
    for (i, q) in quests.iter().enumerate() {
        assert!(
            q.id.starts_with(&format!("{:02}-", i + 1)),
            "{} is out of order",
            q.id
        );
        assert!(q.steps.len() >= 5, "{} is short", q.id);
    }
}

#[test]
fn quests_cover_the_prd_list() {
    let slugs: Vec<String> = shipped().iter().map(|q| q.slug().to_owned()).collect();
    for want in [
        "first-patch",
        "audio-reactive",
        "first-clip-deck",
        "kernel-dsl",
        "particles",
        "ai-clip",
        "mapping",
        "dmx",
        "packs",
        "comfyui",
    ] {
        assert!(slugs.iter().any(|s| s == want), "no quest {want}");
    }
}

/// Every operator and parameter the quests name exists in the registry the
/// app builds. ComfyUI workflow operators come from the example project.
#[test]
fn quests_name_only_operators_and_parameters_that_exist() {
    let mut registry = crate::app::builtin_registry();
    let comfy = lsr_ops_ai::comfy::ProjectComfy::open(&root().join("examples/comfy-invert.lasair"));
    assert!(comfy.errors().is_empty());
    for schema in comfy.schemas() {
        registry
            .replace_custom(schema.clone(), COMFY_CUSTOM)
            .unwrap();
    }
    let mut problems = Vec::new();
    for q in shipped() {
        problems.extend(q.check_against(&registry));
        if let Some(start) = &q.start {
            assert!(root().join(start).is_dir(), "{}: no {start}", q.id);
        }
    }
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn quest_text_names_no_other_product_or_operator_family() {
    // CLAUDE.md rule 10.
    const BANNED: [&str; 12] = [
        "touchdesigner",
        "resolume",
        "houdini",
        "notch",
        "vvvv",
        "max/msp",
        "ableton",
        "wrangle",
        "vex",
        "isadora",
        "madmapper",
        "qlab",
    ];
    for (id, source) in SHIPPED {
        let lower = source.to_lowercase();
        for word in BANNED {
            let found = lower
                .split(|c: char| !c.is_alphanumeric() && c != '/')
                .any(|w| w == word);
            assert!(!found, "{id} mentions {word}");
        }
        for family in ["TOP", "CHOP", "SOP", "POP", "COMP"] {
            let found = source
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == family);
            assert!(!found, "{id} uses the family name {family}");
        }
    }
}

#[test]
fn bad_quest_files_are_refused_with_a_reason() {
    let ok = r#"
title = "T"
summary = "S"
minutes = 5
[[step]]
title = "A"
text = "do it"
check = { kind = "node_exists", type = "img.noise" }
"#;
    assert!(Quest::parse("01-t", ok).is_ok());
    for (bad, why) in [
        (ok.replace("node_exists", "teleport"), "unknown check kind"),
        (ok.replace("minutes = 5", "minutes = 0"), "minutes"),
        (ok.replace("title = \"T\"", ""), "title"),
        (
            ok.replace("[[step]]", "colour = 1\n[[step]]"),
            "unknown key",
        ),
        (ok.replace("text = \"do it\"", "text = \"\""), "text"),
        (
            ok.replace("text = \"do it\"", "text = \"x\"\nsecret = 1"),
            "unknown key",
        ),
        (ok.split("[[step]]").next().unwrap().to_owned(), "[[step]]"),
    ] {
        let e = Quest::parse("01-t", &bad).unwrap_err();
        assert!(e.contains(why), "{e} should mention {why}");
    }
    // Names that do not exist in the registry are found.
    let wrong = Quest::parse("01-t", &ok.replace("img.noise", "img.nosie")).unwrap();
    let problems = wrong.check_against(&crate::app::builtin_registry());
    assert!(
        problems.iter().any(|p| p.contains("img.nosie")),
        "{problems:?}"
    );
    let wrong_param = Quest::parse(
        "01-t",
        &ok.replace(
            r#"{ kind = "node_exists", type = "img.noise" }"#,
            r#"{ kind = "param", type = "img.noise", name = "scal", op = "changed" }"#,
        ),
    )
    .unwrap();
    assert!(
        wrong_param
            .check_against(&crate::app::builtin_registry())
            .iter()
            .any(|p| p.contains("scal"))
    );
}

// ---- each check kind ----

#[test]
fn node_exists_and_wired() {
    let mut w = World::new();
    let exists = parse(r#"{ kind = "node_exists", type = "img.noise" }"#);
    let wired = parse(
        r#"{ kind = "wired", from_type = "img.noise", to_type = "img.level", input = "in" }"#,
    );
    let any_input = parse(r#"{ kind = "wired", from_type = "img.noise", to_type = "img.level" }"#);
    assert!(!w.eval(&exists) && !w.eval(&wired));
    let noise = w.add("img.noise");
    assert!(w.eval(&exists) && !w.eval(&wired));
    let level = w.add("img.level");
    assert!(!w.eval(&wired), "nodes without a wire are not wired");
    let other = w.add("img.level");
    w.wire(noise, other, "in");
    assert!(w.eval(&wired) && w.eval(&any_input));
    let wrong_direction =
        parse(r#"{ kind = "wired", from_type = "img.level", to_type = "img.noise" }"#);
    assert!(!w.eval(&wrong_direction));
    let _ = level;
    // The right types but the wrong socket.
    let mixer = w.add("img.mixer");
    let layer = w.add("img.layer");
    w.wire(layer, mixer, "layer1");
    assert!(!w.eval(&parse(
        r#"{ kind = "wired", from_type = "img.layer", to_type = "img.mixer", input = "layer0" }"#
    )));
    assert!(w.eval(&parse(
        r#"{ kind = "wired", from_type = "img.layer", to_type = "img.mixer", input = "layer1" }"#
    )));
}

#[test]
fn param_ops() {
    let mut w = World::new();
    let level = w.add("img.level");
    let lfo = w.add("sig.lfo");
    let check = |op: &str| {
        parse(&format!(
            r#"{{ kind = "param", type = "img.level", name = "contrast", op = {op} }}"#
        ))
    };
    let gt = check(r#""gt", value = 1.0"#);
    let lt = check(r#""lt", value = 1.0"#);
    let eq = check(r#""eq", value = 1.5"#);
    let changed = check(r#""changed""#);
    let bound = check(r#""bound""#);
    let expr = check(r#""expr""#);
    // Defaults: contrast is 1.0, nothing set.
    assert!(!w.eval(&gt) && !w.eval(&lt) && !w.eval(&changed) && !w.eval(&eq));
    assert!(!w.eval(&bound) && !w.eval(&expr));
    w.set(level, "contrast", json!(1.5));
    assert!(w.eval(&gt) && !w.eval(&lt) && w.eval(&eq) && w.eval(&changed));
    w.set(level, "contrast", json!(0.5));
    assert!(!w.eval(&gt) && w.eval(&lt) && !w.eval(&eq) && w.eval(&changed));
    // Setting a parameter back to its default is not a change.
    w.set(level, "contrast", json!(1));
    assert!(!w.eval(&changed) && !w.eval(&gt) && !w.eval(&lt));
    w.bind(level, "contrast", lfo, "lfo");
    assert!(w.eval(&bound) && !w.eval(&expr));
    w.edit(Edit::SetParam {
        node: level,
        param: "contrast".into(),
        value: Some(Param {
            value: json!(1.0),
            expr: Some("1 + time".into()),
            bind: None,
        }),
    });
    assert!(w.eval(&expr) && !w.eval(&bound));

    // eq on text and toggles; contains on text.
    let layer = w.add("img.layer");
    let fade = parse(
        r#"{ kind = "param", type = "img.layer", name = "transition", op = "eq", value = "fade" }"#,
    );
    assert!(!w.eval(&fade));
    w.set(layer, "transition", json!("fade"));
    assert!(w.eval(&fade));
    let k = w.add("pnt.kernel");
    let has = parse(
        r#"{ kind = "param", type = "pnt.kernel", name = "code", op = "contains", value = "ch(" }"#,
    );
    assert!(!w.eval(&has));
    w.set(k, "code", json!("@P.y = ch(\"a\", 1.0);"));
    assert!(w.eval(&has));
    // The wrong type of node never satisfies a check.
    assert!(!w.eval(&parse(
        r#"{ kind = "param", type = "img.blur", name = "radius", op = "changed" }"#
    )));
}

#[test]
fn cooks_without_error() {
    let mut w = World::new();
    let check = parse(r#"{ kind = "cooks_without_error", type = "img.level" }"#);
    assert!(!w.eval(&check), "no node");
    let a = w.add("img.level");
    assert!(!w.eval(&check), "never cooked");
    w.cooked(a);
    assert!(w.eval(&check));
    w.failed(a, "boom");
    assert!(!w.eval(&check), "an error fails it");
    w.cooked(a);
    let b = w.add("img.level");
    w.failed(b, "boom");
    assert!(!w.eval(&check), "any node with an error fails it");
}

#[test]
fn stage_layer_playing() {
    let mut w = World::new();
    let check = parse(r#"{ kind = "stage_layer_playing" }"#);
    assert!(!w.eval(&check), "no deck");
    let mixer = w.add("img.mixer");
    let layer = w.add("img.layer");
    w.wire(layer, mixer, "layer0");
    let clip = w.add("img.noise");
    w.wire(clip, layer, "clip0");
    assert!(!w.eval(&check), "a deck, nothing launched");
    w.edit(lsr_stage::set_target(layer, Some(1)));
    assert!(!w.eval(&check), "slot 1 has no clip");
    w.edit(lsr_stage::set_target(layer, Some(0)));
    assert!(w.eval(&check));
    w.edit(lsr_stage::set_target(layer, None));
    assert!(!w.eval(&check), "stopped");
    // A step that needs the Stage model says so.
    assert!(Check::StageLayerPlaying.needs_stage());
    assert!(!parse(r#"{ kind = "kernel_compiles" }"#).needs_stage());
    // Without a Stage model the check cannot hold.
    w.edit(lsr_stage::set_target(layer, Some(0)));
    assert!(w.eval(&check));
    let snapshot = Snapshot {
        network: &w.net,
        registry: &w.registry,
        cook: &w.cooks,
        stage: None,
        mappings: &w.mappings,
        pack_instances: &[],
        project_dir: None,
    };
    assert!(!check.eval(&snapshot));
}

#[test]
fn mapping_exists() {
    let mut w = World::new();
    let check = parse(r#"{ kind = "mapping_exists", param = "brightness" }"#);
    let level = w.add("img.level");
    assert!(!w.eval(&check));
    let osc = Source::Osc {
        address: "/fader/1".into(),
        arg: 0,
    };
    let target = |node, param: &str| Target {
        node,
        param: param.into(),
        component: None,
    };
    w.mappings
        .add(osc.clone(), target(level, "contrast"), [0.0, 2.0]);
    assert!(!w.eval(&check), "a mapping of another parameter");
    let id = w
        .mappings
        .add(osc.clone(), target(level, "brightness"), [0.0, 2.0]);
    assert!(w.eval(&check));
    w.mappings.remove(id);
    assert!(!w.eval(&check));
    // A mapping of a node that was deleted does not count.
    w.mappings
        .add(osc, target(NodeId(999), "brightness"), [0.0, 1.0]);
    assert!(!w.eval(&check));
}

#[test]
fn kernel_compiles() {
    let mut w = World::new();
    let check = parse(r#"{ kind = "kernel_compiles" }"#);
    assert!(!w.eval(&check), "no kernel");
    let k = w.add("pnt.kernel");
    assert!(w.eval(&check), "the default code compiles");
    w.set(k, "code", json!("@P.y = ;"));
    assert!(!w.eval(&check), "a syntax error");
    w.set(k, "code", json!("@P.y = sin(@P.x);"));
    assert!(w.eval(&check));
    let pixels = w.add("img.kernel");
    assert!(w.eval(&check), "the default pixel kernel compiles");
    w.set(pixels, "code", json!("@Cd.r = nonsense(1.0);"));
    assert!(!w.eval(&check), "any broken kernel fails the check");
    // A kernel read from a file must have cooked without an error.
    w.set(pixels, "code", json!(""));
    w.set(pixels, "file", json!("kernels/grade.pixels.atk"));
    assert!(!w.eval(&check), "not cooked yet");
    w.cooked(pixels);
    assert!(w.eval(&check));
}

#[test]
fn pack_instance_and_file_exists() {
    let mut w = World::new();
    let pack = parse(r#"{ kind = "pack_instance", name = "beat_flash" }"#);
    assert!(!w.eval(&pack));
    w.packs = vec!["beat_flash".into()];
    assert!(w.eval(&pack));
    assert!(!w.eval(&parse(r#"{ kind = "pack_instance", name = "other" }"#)));

    let file = parse(r#"{ kind = "file_exists", path = "patch.toml" }"#);
    assert!(!w.eval(&file), "no project folder");
    let dir = std::env::temp_dir().join(format!("lsr-learn-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    w.dir = Some(dir.clone());
    assert!(!w.eval(&file));
    std::fs::write(dir.join("patch.toml"), "x").unwrap();
    assert!(w.eval(&file));
    std::fs::create_dir_all(dir.join("folder")).unwrap();
    assert!(
        !w.eval(&parse(r#"{ kind = "file_exists", path = "folder" }"#)),
        "a folder is not a file"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn project_operators_match_by_prefix() {
    let mut registry = crate::app::builtin_registry();
    let dir = root().join("examples/comfy-invert.lasair");
    let comfy = lsr_ops_ai::comfy::ProjectComfy::open(&dir);
    for s in comfy.schemas() {
        registry.replace_custom(s.clone(), COMFY_CUSTOM).unwrap();
    }
    let mut w = World::with(registry, Network::default());
    let exists = parse(r#"{ kind = "node_exists", type = "comfy.*" }"#);
    assert!(!w.eval(&exists));
    w.add("comfy.invert");
    assert!(w.eval(&exists));
    assert!(!w.eval(&parse(r#"{ kind = "node_exists", type = "py.*" }"#)));
}

// ---- scripted students ----

/// Plays a quest: each action must finish exactly the steps expected, and
/// the quest must not move before the student acts.
struct Student {
    quest: Quest,
    done: usize,
}

impl Student {
    fn new(id: &str) -> Self {
        Self {
            quest: quest(id),
            done: 0,
        }
    }

    /// The quest has not moved on its own.
    fn waiting(&mut self, w: &World) {
        let before = self.done;
        w.advance(&self.quest, &mut self.done);
        assert_eq!(
            self.done,
            before,
            "{}: step {} was already satisfied before the student acted",
            self.quest.id,
            before + 1
        );
    }

    /// The student has just opened the project: `finished` steps are
    /// already satisfied.
    fn opens(&mut self, w: &World, finished: usize) {
        w.advance(&self.quest, &mut self.done);
        assert_eq!(
            self.done, finished,
            "{}: steps done on opening",
            self.quest.id
        );
    }

    /// Acts, and expects `finished` steps done afterwards.
    fn act_to(&mut self, w: &mut World, finished: usize, f: impl FnOnce(&mut World)) {
        self.waiting(w);
        f(w);
        w.advance(&self.quest, &mut self.done);
        assert_eq!(
            self.done,
            finished,
            "{}: after the action for step {} ('{}')",
            self.quest.id,
            self.done + 1,
            self.quest.steps[self.done.min(self.quest.steps.len() - 1)].title
        );
    }

    /// Acts and finishes the next step.
    fn act(&mut self, w: &mut World, f: impl FnOnce(&mut World)) {
        let next = self.done + 1;
        self.act_to(w, next, f);
    }

    fn finish(&self) {
        assert_eq!(
            self.done,
            self.quest.steps.len(),
            "{} unfinished",
            self.quest.id
        );
    }
}

#[test]
fn student_finishes_first_patch() {
    let mut w = World::new();
    let mut s = Student::new("01-first-patch");
    s.act(&mut w, |w| {
        w.add("img.noise");
    });
    s.act(&mut w, |w| {
        let (noise, kal) = (w.by_type("img.noise"), w.add("img.kaleidoscope"));
        w.wire(noise, kal, "in");
    });
    s.act(&mut w, |w| {
        let kal = w.by_type("img.kaleidoscope");
        w.set(kal, "segments", json!(9));
    });
    s.act(&mut w, |w| {
        let (kal, level) = (w.by_type("img.kaleidoscope"), w.add("img.level"));
        w.wire(kal, level, "in");
    });
    s.act(&mut w, |w| {
        let level = w.by_type("img.level");
        w.cooked(level);
    });
    s.act(&mut w, |w| {
        let level = w.by_type("img.level");
        w.set(level, "contrast", json!(1.4));
    });
    s.act(&mut w, |w| {
        w.add("sig.lfo");
    });
    s.act(&mut w, |w| {
        let (noise, lfo) = (w.by_type("img.noise"), w.by_type("sig.lfo"));
        w.bind(noise, "z", lfo, "lfo");
    });
    s.finish();
}

#[test]
fn student_finishes_audio_reactive() {
    let mut w = World::new();
    let mut s = Student::new("02-audio-reactive");
    s.act(&mut w, |w| {
        w.add("sig.audio_file_in");
    });
    s.act(&mut w, |w| {
        let a = w.by_type("sig.audio_file_in");
        w.set(a, "file", json!("/music/beat.wav"));
    });
    s.act(&mut w, |w| {
        let (a, sp) = (w.by_type("sig.audio_file_in"), w.add("sig.spectrum"));
        w.wire(a, sp, "in");
    });
    s.act(&mut w, |w| {
        let (n, f) = (w.add("img.noise"), w.add("img.feedback"));
        w.wire(n, f, "in");
    });
    s.act(&mut w, |w| {
        let (f, t) = (w.by_type("img.feedback"), w.add("img.transform"));
        w.wire(f, t, "in");
    });
    s.act(&mut w, |w| {
        let t = w.by_type("img.transform");
        w.set(t, "scale", json!([1.02, 1.02]));
        w.set(t, "extend", json!("mirror"));
    });
    s.act(&mut w, |w| {
        let (n, t, c) = (
            w.by_type("img.noise"),
            w.by_type("img.transform"),
            w.add("img.composite"),
        );
        w.wire(n, c, "a");
        w.wire(t, c, "b");
    });
    s.act(&mut w, |w| {
        let (c, f) = (w.by_type("img.composite"), w.by_type("img.feedback"));
        w.wire(c, f, "loop");
    });
    s.act(&mut w, |w| {
        let (t, sp) = (w.by_type("img.transform"), w.by_type("sig.spectrum"));
        w.bind(t, "rotate", sp, "band2");
    });
    // A file that cannot be read keeps the last step open.
    let audio = w.by_type("sig.audio_file_in");
    w.failed(audio, "no such file");
    s.waiting(&w);
    s.act(&mut w, |w| w.cooked(audio));
    s.finish();
}

#[test]
fn student_finishes_first_clip_deck() {
    let mut w = World::new();
    let mut s = Student::new("03-first-clip-deck");
    s.act(&mut w, |w| {
        w.add("img.mixer");
    });
    s.act(&mut w, |w| {
        let (m, l) = (w.by_type("img.mixer"), w.add("img.layer"));
        w.wire(l, m, "layer0");
    });
    s.act(&mut w, |w| {
        let (l, n) = (w.by_type("img.layer"), w.add("img.noise"));
        w.wire(n, l, "clip0");
    });
    s.act(&mut w, |w| {
        let (l, r) = (w.by_type("img.layer"), w.add("img.ramp"));
        w.wire(r, l, "clip1");
    });
    s.act(&mut w, |w| {
        let l = w.by_type("img.layer");
        w.edit(lsr_stage::set_target(l, Some(0)));
    });
    s.act(&mut w, |w| {
        let l = w.by_type("img.layer");
        w.set(l, "transition", json!("fade"));
    });
    s.act(&mut w, |w| {
        let l = w.by_type("img.layer");
        w.edit(lsr_stage::set_target(l, Some(1)));
    });
    s.finish();
}

#[test]
fn student_finishes_kernel_dsl() {
    let mut w = World::new();
    let mut s = Student::new("04-kernel-dsl");
    s.act(&mut w, |w| {
        w.add("pnt.points");
    });
    s.act(&mut w, |w| {
        let (p, k) = (w.by_type("pnt.points"), w.add("pnt.kernel"));
        w.wire(p, k, "in");
    });
    s.act(&mut w, |w| {
        let (k, r) = (w.by_type("pnt.kernel"), w.add("img.render_points"));
        w.wire(k, r, "geo");
    });
    s.act(&mut w, |w| {
        let r = w.by_type("img.render_points");
        w.cooked(r);
    });
    // The kernel the quest tells the student to write compiles as printed.
    let code = quest("04-kernel-dsl")
        .steps
        .iter()
        .find(|st| st.title == "Write your own kernel")
        .map(|st| {
            let text = &st.text;
            let a = text.find("```").unwrap() + 3;
            let b = text[a..].find("```").unwrap() + a;
            text[a..b].trim().to_owned()
        })
        .unwrap();
    // Typing it with a mistake does not finish the compile step.
    s.act_to(&mut w, 5, |w| {
        let k = w.by_type("pnt.kernel");
        w.set(k, "code", json!(format!("{code}\nfloat oops = ;")));
    });
    s.act(&mut w, |w| {
        let k = w.by_type("pnt.kernel");
        w.set(k, "code", json!(code));
    });
    s.act(&mut w, |w| {
        let (n, k) = (w.add("img.noise"), w.add("img.kernel"));
        w.wire(n, k, "in0");
    });
    s.act(&mut w, |w| {
        let k = w.by_type("img.kernel");
        w.cooked(k);
    });
    s.finish();
}

#[test]
fn student_finishes_particles() {
    let mut w = World::new();
    let mut s = Student::new("05-particles");
    s.act(&mut w, |w| {
        w.add("pnt.particles");
    });
    s.act(&mut w, |w| {
        let (p, r) = (w.by_type("pnt.particles"), w.add("img.render_points"));
        w.wire(p, r, "geo");
    });
    s.act(&mut w, |w| {
        let r = w.by_type("img.render_points");
        w.cooked(r);
    });
    s.act(&mut w, |w| {
        let p = w.by_type("pnt.particles");
        w.set(p, "rate", json!(20000.0));
    });
    s.act(&mut w, |w| {
        let p = w.by_type("pnt.particles");
        w.set(p, "gravity", json!([0.0, -0.5, 0.0]));
    });
    s.act(&mut w, |w| {
        let p = w.by_type("pnt.particles");
        w.set(p, "curl_amplitude", json!(3.0));
    });
    s.act(&mut w, |w| {
        let p = w.by_type("pnt.particles");
        w.set(p, "colour_birth", json!([0.2, 1.0, 0.5, 1.0]));
    });
    s.act(&mut w, |w| {
        let (p, l) = (w.by_type("pnt.particles"), w.add("sig.lfo"));
        w.bind(p, "attract", l, "lfo");
    });
    s.finish();
}

#[test]
fn student_finishes_ai_clip() {
    let mut w = World::new();
    let mut s = Student::new("06-ai-clip");
    s.act(&mut w, |w| {
        let (m, l) = (w.add("img.mixer"), w.add("img.layer"));
        w.wire(l, m, "layer0");
    });
    s.act(&mut w, |w| {
        let (l, p) = (w.by_type("img.layer"), w.add("ai.prompt"));
        w.wire(p, l, "clip0");
    });
    s.act(&mut w, |w| {
        let (p, n) = (w.by_type("ai.prompt"), w.add("img.noise"));
        w.wire(n, p, "image");
    });
    s.act(&mut w, |w| {
        let p = w.by_type("ai.prompt");
        w.set(p, "prompt", json!("a neon jellyfish"));
    });
    s.act(&mut w, |w| {
        let (m, d) = (w.by_type("img.mixer"), w.add("ai.diffusion"));
        w.wire(m, d, "image");
    });
    s.act(&mut w, |w| {
        let (m, d) = (w.by_type("img.mixer"), w.by_type("ai.diffusion"));
        let name = w.net.node(m).unwrap().name.clone();
        w.set(d, "prompts_from", json!(name));
    });
    s.act(&mut w, |w| {
        let l = w.by_type("img.layer");
        w.edit(lsr_stage::set_target(l, Some(0)));
    });
    s.act(&mut w, |w| {
        let (m, a) = (w.by_type("img.mixer"), w.add("ai.model"));
        w.wire(m, a, "image");
        w.cooked(a);
    });
    s.finish();
}

#[test]
fn student_finishes_mapping() {
    let mut w = World::new();
    let mut s = Student::new("07-mapping");
    s.act(&mut w, |w| {
        let (n, l) = (w.add("img.noise"), w.add("img.level"));
        w.wire(n, l, "in");
    });
    let level = w.by_type("img.level");
    let osc = |a: &str| Source::Osc {
        address: a.into(),
        arg: 0,
    };
    s.act(&mut w, |w| {
        w.mappings.add(
            osc("/fader/1"),
            Target {
                node: level,
                param: "brightness".into(),
                component: None,
            },
            [0.0, 2.0],
        );
    });
    s.act(&mut w, |w| {
        w.mappings.add(
            osc("/fader/2"),
            Target {
                node: level,
                param: "contrast".into(),
                component: None,
            },
            [0.0, 2.0],
        );
    });
    s.act(&mut w, |w| {
        w.add("sig.osc_in");
    });
    s.act(&mut w, |w| {
        let o = w.by_type("sig.osc_in");
        w.bind(level, "gamma", o, "fader_1");
    });
    s.finish();
}

#[test]
fn student_finishes_dmx() {
    let dir = std::env::temp_dir().join(format!("lsr-learn-dmx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut w = World::new();
    let mut s = Student::new("08-dmx");
    s.act(&mut w, |w| {
        w.add("sig.lfo");
    });
    s.act(&mut w, |w| {
        let (l, d) = (w.by_type("sig.lfo"), w.add("sig.dmx_out"));
        w.wire(l, d, "in");
    });
    s.act(&mut w, |w| {
        let d = w.by_type("sig.dmx_out");
        w.set(d, "host", json!("127.0.0.1"));
    });
    s.act(&mut w, |w| {
        let d = w.by_type("sig.dmx_out");
        w.cooked(d);
    });
    s.act(&mut w, |w| {
        // Save As makes the folder and project.toml.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project.toml"), "format = 1\n").unwrap();
        w.dir = Some(dir.clone());
    });
    // The patch file the quest prints is a valid patch.
    let patch = quest("08-dmx")
        .steps
        .iter()
        .find(|st| st.title == "Write a patch file")
        .map(|st| {
            let a = st.text.find("```").unwrap() + 3;
            let b = st.text[a..].find("```").unwrap() + a;
            st.text[a..b].trim().to_owned()
        })
        .unwrap();
    lsr_io::fixture::parse_patch(&patch, |id| {
        lsr_io::fixture::builtin(id).ok_or_else(|| format!("no fixture type {id}"))
    })
    .unwrap_or_else(|e| panic!("the quest's patch.toml is refused: {e}"));
    s.act(&mut w, |_| {
        std::fs::write(dir.join("patch.toml"), &patch).unwrap()
    });
    s.act(&mut w, |w| {
        let (n, p) = (w.add("img.noise"), w.add("img.pixel_map"));
        w.wire(n, p, "in");
    });
    s.act(&mut w, |w| {
        let p = w.by_type("img.pixel_map");
        w.set(p, "patch", json!("patch.toml"));
    });
    s.act(&mut w, |w| {
        let p = w.by_type("img.pixel_map");
        w.cooked(p);
    });
    s.finish();
    std::fs::remove_dir_all(dir).unwrap();
}

/// What the app shows after opening a project with Packs: the patch
/// expanded, and the Packs it uses found from the expanded names.
fn open_example(name: &str) -> (World, PathBuf, lsr_format::Project) {
    let dir = root().join("examples").join(name);
    let mut project = lsr_format::load(&dir).unwrap();
    let base = crate::app::builtin_registry();
    let mut registry = base.clone();
    let comfy = lsr_ops_ai::comfy::ProjectComfy::open(&dir);
    for s in comfy.schemas() {
        registry.replace_custom(s.clone(), COMFY_CUSTOM).unwrap();
    }
    let registry = project.packs.register(&registry).unwrap();
    let net = project.networks.remove(lsr_format::MAIN).unwrap();
    let authored = net.validated(&registry).unwrap();
    let net = if project.packs.is_empty() {
        authored
    } else {
        project.packs.flatten(&authored, &registry).unwrap().network
    };
    let mut w = World::with(registry, net);
    w.packs = pack_instances(&w.net, &project.packs);
    w.dir = Some(dir.clone());
    (w, dir, project)
}

#[test]
fn student_finishes_packs() {
    let (mut w, _, project) = open_example("pack-beat-flash.lasair");
    assert_eq!(w.packs, ["beat_flash"]);
    // Opening the project finishes the first two steps.
    let mut s = Student::new("09-packs");
    s.opens(&w, 2);
    s.act(&mut w, |w| {
        let clock = w.by_type("sig.beat");
        w.set(clock, "bpm", json!(150.0));
    });
    // Binding a new Transform to the Pack's flash and viewing it finishes
    // the next two; the Pack's own help ships with the project.
    s.act_to(&mut w, 6, |w| {
        let boost = w
            .net
            .nodes()
            .find(|n| n.name.ends_with("__boost"))
            .unwrap()
            .id;
        let t = w.add("img.transform");
        w.bind(t, "rotate", boost, "out");
        w.cooked(t);
    });
    s.finish();
    assert!(project.packs.get("beat_flash").is_some());
}

#[test]
fn pack_instances_are_found_only_for_packs_the_project_uses() {
    let (w, _, project) = open_example("pack-kaleido-tunnel.lasair");
    assert_eq!(w.packs, ["kaleido_tunnel"]);
    let (plain, _, none) = open_example("particles.lasair");
    assert!(plain.packs.is_empty() && none.packs.is_empty());
    // Two Packs in one project are told apart.
    let (both, _, _) = open_example("pack-audio-pulse.lasair");
    assert_eq!(both.packs, ["audio_pulse"]);
    let _ = project;
}

#[test]
fn student_finishes_comfyui() {
    let (mut w, _, _) = open_example("comfy-invert.lasair");
    let mut s = Student::new("10-comfyui");
    // Opening the example finishes the first step; the node is not cooked.
    s.opens(&w, 1);
    s.act(&mut w, |w| {
        let c = w.by_type("comfy.invert");
        w.cooked(c);
    });
    s.act(&mut w, |w| {
        let (c, n) = (w.by_type("comfy.invert"), w.add("img.noise"));
        w.wire(n, c, "in0");
    });
    s.act(&mut w, |w| {
        let c = w.by_type("comfy.invert");
        w.set(c, "auto_run", json!(false));
    });
    s.act(&mut w, |w| {
        let c = w.by_type("comfy.invert");
        w.set(c, "auto_run", json!(true));
    });
    s.finish();
}

// ---- progress and the panel ----

#[test]
fn the_panel_resumes_saves_and_restarts() {
    let dir = std::env::temp_dir().join(format!("lsr-learn-panel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("learn.toml");
    let mut panel = LearnPanel::with(shipped(), Progress::load(file.clone()));
    assert_eq!(
        panel.wants_tick(0.0),
        None,
        "nothing open, nothing to check"
    );
    panel.open_quest(0);
    assert_eq!(panel.wants_tick(0.0), Some(false));

    let mut w = World::new();
    w.add("img.noise");
    let stage = Stage::default();
    let tick = |panel: &mut LearnPanel, w: &World, now: f64| {
        let snapshot = Snapshot {
            network: &w.net,
            registry: &w.registry,
            cook: &w.cooks,
            stage: Some(&stage),
            mappings: &w.mappings,
            pack_instances: &[],
            project_dir: None,
        };
        panel.tick(now, &snapshot);
    };
    tick(&mut panel, &w, 1.0);
    assert_eq!(panel.done, 1);
    assert_eq!(panel.wants_tick(1.1), None);
    assert!(panel.wants_tick(1.5).is_some());

    // A new session picks up where the student stopped.
    let mut again = LearnPanel::with(shipped(), Progress::load(file.clone()));
    again.open_quest(0);
    assert_eq!(again.done, 1);
    // Changing the patch afterwards does not take progress back.
    let noise = w.by_type("img.noise");
    w.edit(Edit::RemoveNode { id: noise });
    tick(&mut again, &w, 2.0);
    assert_eq!(again.done, 1);

    // Another quest keeps its own count.
    again.open_quest(1);
    assert_eq!(again.done, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_finished_quest_is_complete_in_the_progress_file() {
    let mut progress = Progress::in_memory();
    let q = quest("07-mapping");
    assert!(!progress.is_complete(&q));
    progress.set(&q.id, q.steps.len());
    assert!(progress.is_complete(&q));
}

#[test]
fn panel_text_uses_only_glyphs_the_default_fonts_have() {
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    let mut texts = vec!["\u{2022} item".to_owned(), "< All quests".to_owned()];
    // Everything the quests print, line by line.
    for q in shipped() {
        texts.push(q.title.clone());
        texts.push(q.summary.clone());
        for s in &q.steps {
            texts.push(s.title.clone());
            texts.extend(s.text.lines().map(str::to_owned));
            texts.extend(s.hint.clone());
        }
    }
    ctx.fonts_mut(|f| {
        for t in &texts {
            assert!(
                f.has_glyphs(&egui::FontId::proportional(12.0), t)
                    || f.has_glyphs(&egui::FontId::monospace(12.0), t),
                "{t:?} has a glyph the default fonts lack"
            );
        }
    });
}

#[test]
fn examples_resolve_from_the_source_tree() {
    let p = resolve_example("examples/comfy-invert.lasair").unwrap();
    assert!(p.is_dir());
    assert!(resolve_example("examples/not-there.lasair").is_none());
}
