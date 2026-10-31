use super::*;
use atd_core::graph::{ChannelRef, Comment, Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::{ParamKind, Registry, WireType};
use serde_json::json;

fn registry() -> Registry {
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    reg
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

/// Awkward strings: quotes, `'''`, backslashes, tabs, CRLF, Unicode.
const STRINGS: [&str; 9] = [
    "",
    "plain",
    "line one\nline two\n",
    "ends with a quote'",
    "has ''' inside\nand more",
    "back\\slash \"quoted\"\ttab",
    "crlf\r\nline",
    "émoji ✨ and \u{7f} delete",
    "\n\nleading blank lines",
];

/// A random valid network over every IMG and SIG operator.
fn random_network(reg: &Registry, rng: &mut Rng) -> Network {
    let types: Vec<String> = reg.iter().map(|s| s.type_id.clone()).collect();
    let mut net = Network::default();
    for _ in 0..1 + rng.next(25) {
        let ty = &types[rng.next(types.len() as u64) as usize];
        let id = net.alloc_id();
        let mut node = Node::new(id, ty.clone(), net.unique_name("n"));
        node.pos = [
            (rng.next(2000) as f32 - 1000.0) / 7.0,
            (rng.next(2000) as f32 - 1000.0) / 3.0,
        ];
        net.apply(reg, Edit::AddNode { node }).unwrap();
        let schema = reg.get(ty).unwrap();
        for p in &schema.params {
            if rng.next(3) != 0 {
                continue;
            }
            let value = match &p.kind {
                ParamKind::Float { min, max } => {
                    let (lo, hi) = (min.unwrap_or(-10.0), max.unwrap_or(10.0));
                    json!(lo + (hi - lo) * rng.next(1000) as f64 / 999.0)
                }
                ParamKind::Int { min, max } => {
                    let (lo, hi) = (min.unwrap_or(-5), max.unwrap_or(5));
                    json!(lo + rng.next((hi - lo + 1) as u64) as i64)
                }
                ParamKind::Menu { options } => {
                    json!(options[rng.next(options.len() as u64) as usize])
                }
                ParamKind::Toggle | ParamKind::Pulse => json!(rng.next(2) == 1),
                ParamKind::Colour => json!([0.1, 0.25, 1.0, rng.next(2) as f64]),
                ParamKind::Vec { n } => json!(vec![0.5; usize::from(*n)]),
                ParamKind::String | ParamKind::Path => {
                    json!(STRINGS[rng.next(STRINGS.len() as u64) as usize])
                }
            };
            let _ = net.apply(
                reg,
                Edit::SetParam {
                    node: id,
                    param: p.name.clone(),
                    value: Some(Param::constant(value)),
                },
            );
        }
    }
    let ids: Vec<NodeId> = net.nodes().map(|n| n.id).collect();
    for _ in 0..rng.next(30) {
        let (a, b) = (
            ids[rng.next(ids.len() as u64) as usize],
            ids[rng.next(ids.len() as u64) as usize],
        );
        let (sa, sb) = (
            reg.get(&net.node(a).unwrap().type_id).unwrap(),
            reg.get(&net.node(b).unwrap().type_id).unwrap(),
        );
        if let (Some(out), Some(input)) = (
            sa.outputs.first(),
            sb.inputs.get(rng.next(4) as usize % sb.inputs.len().max(1)),
        ) {
            let _ = net.apply(
                reg,
                Edit::Connect {
                    wire: Wire::new(Endpoint::new(a, &out.name), Endpoint::new(b, &input.name)),
                },
            );
        }
        // Bind a float to a SIG node's channel.
        if sa.outputs.first().is_some_and(|o| o.wire == WireType::Sig)
            && let Some(p) = sb
                .params
                .iter()
                .find(|p| matches!(p.kind, ParamKind::Float { .. }))
        {
            let channel = ["", "chan1", "x y"][rng.next(3) as usize];
            let _ = net.apply(
                reg,
                Edit::SetParam {
                    node: b,
                    param: p.name.clone(),
                    value: Some(Param {
                        value: json!(0.5),
                        expr: (rng.next(3) == 0).then(|| "me.digits * 2\n# note".to_owned()),
                        bind: Some(ChannelRef {
                            node: a,
                            channel: channel.into(),
                        }),
                    }),
                },
            );
        }
    }
    for _ in 0..rng.next(3) {
        let id = net.alloc_comment_id();
        let text = STRINGS[rng.next(STRINGS.len() as u64) as usize].to_owned();
        net.apply(
            reg,
            Edit::AddComment {
                comment: Comment {
                    id,
                    text,
                    pos: [1.5, -2.25],
                    size: [300.0, 120.0],
                },
            },
        )
        .unwrap();
    }
    net
}

#[test]
fn random_networks_round_trip_exactly_and_byte_stable() {
    let reg = registry();
    let mut rng = Rng(0x5eed_f00d_cafe_d00d);
    for round in 0..300 {
        let net = random_network(&reg, &mut rng);
        let text = network_to_string(&net).unwrap();
        let back = network_from_str(&text).unwrap_or_else(|e| panic!("round {round}: {e}\n{text}"));
        assert_eq!(back, net, "round {round}:\n{text}");
        assert_eq!(network_to_string(&back).unwrap(), text, "round {round}");
        back.validated(&reg)
            .unwrap_or_else(|e| panic!("round {round}: {e}\n{text}"));
    }
}

/// Vectors and colours that follow an expression or a channel (A66) save
/// as the same inline table as numbers and round-trip exactly.
#[test]
fn driven_vectors_and_colours_round_trip() {
    let reg = registry();
    let mut net = Network::default();
    let add = |net: &mut Network, ty: &str, name: &str| {
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, ty, name),
            },
        )
        .unwrap();
        id
    };
    let lfo = add(&mut net, "sig.lfo", "lfo1");
    let rgb = add(&mut net, "sig.constant", "rgb");
    let t = add(&mut net, "img.transform", "transform1");
    let c = add(&mut net, "img.constant", "constant1");
    let r = add(&mut net, "img.ramp", "ramp1");
    let set = |net: &mut Network, node, param: &str, value: Param| {
        net.apply(
            &reg,
            Edit::SetParam {
                node,
                param: param.into(),
                value: Some(value),
            },
        )
        .unwrap();
    };
    set(
        &mut net,
        t,
        "scale",
        Param::bound(json!([1.0, 1.0]), lfo, "lfo"),
    );
    set(
        &mut net,
        t,
        "translate",
        Param {
            value: json!([0.0, 0.25]),
            expr: Some("(0.1 * sin(time), 0)".into()),
            bind: None,
        },
    );
    set(
        &mut net,
        c,
        "colour",
        Param::bound(json!([0.0, 0.0, 0.0, 0.5]), rgb, ""),
    );
    set(
        &mut net,
        r,
        "colour_a",
        Param {
            value: json!([1.0, 0.0, 0.0, 1.0]),
            expr: Some("0.5 + 0.5 * sin(time)".into()),
            bind: None,
        },
    );
    let text = network_to_string(&net).unwrap();
    assert!(
        text.contains(
            "params.scale = { value = [1.0, 1.0], bind = { node = 0, channel = \"lfo\" } }"
        ),
        "{text}"
    );
    let back = network_from_str(&text).unwrap();
    assert_eq!(back, net, "{text}");
    assert_eq!(network_to_string(&back).unwrap(), text);
    back.validated(&reg).unwrap();
}

/// DAT nodes (A64): table and script text in parameters (tabs, quotes,
/// `'''`, CRLF) and DAT wires, including a SIG node's DAT output, round-trip
/// exactly and byte-stable.
#[test]
fn dat_text_and_wires_round_trip() {
    use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema};
    let socket = |name: &str, wire| SocketSchema {
        name: name.into(),
        wire,
        optional: true,
        feedback: false,
    };
    let schema = |id: &str, family, inputs, outputs| OperatorSchema {
        type_id: id.into(),
        family,
        label: id.into(),
        summary: "test".into(),
        version: 1,
        params: vec![ParamSchema::string("rows", "Rows", "").multiline()],
        inputs,
        outputs,
        capabilities: vec![],
        cook_budget_us: 1,
        time_dependent: false,
        sink: false,
    };
    let mut reg = Registry::default();
    reg.register(schema(
        "dat.table",
        OpFamily::Dat,
        vec![],
        vec![socket("out", WireType::Dat)],
    ))
    .unwrap();
    reg.register(schema(
        "sig.speech",
        OpFamily::Sig,
        vec![socket("prompt", WireType::Dat)],
        vec![socket("out", WireType::Sig), socket("text", WireType::Dat)],
    ))
    .unwrap();
    reg.register(schema(
        "dat.select",
        OpFamily::Dat,
        vec![socket("in", WireType::Dat)],
        vec![socket("out", WireType::Dat)],
    ))
    .unwrap();
    for (round, text) in STRINGS
        .iter()
        .chain(&[
            "name\tvalue\nsubject\t\"a lighthouse\"\n",
            "def cook(inputs):\n\treturn '''x'''\r\n",
        ])
        .enumerate()
    {
        let mut net = Network::default();
        let ids: Vec<NodeId> = ["dat.table", "sig.speech", "dat.select"]
            .iter()
            .map(|ty| {
                let id = net.alloc_id();
                net.apply(
                    &reg,
                    Edit::AddNode {
                        node: Node::new(id, *ty, net.unique_name("n")),
                    },
                )
                .unwrap();
                net.apply(
                    &reg,
                    Edit::SetParam {
                        node: id,
                        param: "rows".into(),
                        value: Some(Param::constant(json!(text))),
                    },
                )
                .unwrap();
                id
            })
            .collect();
        for (from, out, to, input) in [
            (ids[0], "out", ids[1], "prompt"),
            (ids[1], "text", ids[2], "in"),
        ] {
            net.apply(
                &reg,
                Edit::Connect {
                    wire: Wire::new(Endpoint::new(from, out), Endpoint::new(to, input)),
                },
            )
            .unwrap();
        }
        let saved = network_to_string(&net).unwrap();
        let back =
            network_from_str(&saved).unwrap_or_else(|e| panic!("round {round}: {e}\n{saved}"));
        assert_eq!(back, net, "round {round}:\n{saved}");
        assert_eq!(network_to_string(&back).unwrap(), saved, "round {round}");
        back.validated(&reg).unwrap();
    }
}

const SAMPLE: &str = r#"# Atidarla network. Node IDs are stable: inputs and bindings refer to them.
format = 1
next_id = 5

[[node]]
id = 0
name = "lfo1"
type = "sig.lfo"
pos = [0.0, 0.0]

[[node]]
id = 1
name = "noise1"
type = "img.noise"
pos = [0.0, 120.0]
params.octaves = 6

[[node]]
id = 2
name = "level1"
type = "img.level"
pos = [240.0, 120.0]
inputs.in = { node = 1, output = "out" }
params.brightness = { value = 1.0, bind = { node = 0, channel = "lfo" } }
params.gamma = 1.2

[[node]]
id = 3
name = "text1"
type = "img.text"
pos = [240.0, 240.0]
params.text = '''
Hello
world'''

[[comment]]
id = 4
pos = [-20.0, -40.0]
size = [400.0, 320.0]
text = "Brightness follows the LFO."
"#;

#[test]
fn the_canonical_form_is_stable_and_valid() {
    let reg = registry();
    let net = network_from_str(SAMPLE).unwrap();
    let net = net.validated(&reg).unwrap();
    let level = net.node_by_name("level1").unwrap();
    assert_eq!(
        level.params["brightness"].bind,
        Some(ChannelRef {
            node: NodeId(0),
            channel: "lfo".into()
        })
    );
    assert_eq!(
        net.node_by_name("text1").unwrap().params["text"].value,
        json!("Hello\nworld")
    );
    assert_eq!(net.comments().count(), 1);
    assert_eq!(network_to_string(&net).unwrap(), SAMPLE);
}

#[test]
fn small_edits_are_small_diffs() {
    let reg = registry();
    let base = network_from_str(SAMPLE).unwrap();
    let before = network_to_string(&base).unwrap();
    let changed_lines = |net: &Network| {
        let after = network_to_string(net).unwrap();
        let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(a.len(), b.len(), "same number of lines:\n{after}");
        a.iter()
            .zip(&b)
            .filter(|(x, y)| x != y)
            .map(|(_, y)| y.to_string())
            .collect::<Vec<_>>()
    };
    let mut net = base.clone();
    net.apply(
        &reg,
        Edit::SetParam {
            node: NodeId(2),
            param: "gamma".into(),
            value: Some(Param::constant(json!(1.5))),
        },
    )
    .unwrap();
    assert_eq!(changed_lines(&net), ["params.gamma = 1.5"]);

    let mut net = base.clone();
    net.apply(
        &reg,
        Edit::Rename {
            node: NodeId(1),
            name: "clouds".into(),
        },
    )
    .unwrap();
    assert_eq!(changed_lines(&net), [r#"name = "clouds""#]);

    let mut net = base.clone();
    net.apply(
        &reg,
        Edit::Connect {
            wire: Wire::new(
                Endpoint::new(NodeId(3), "out"),
                Endpoint::new(NodeId(2), "in"),
            ),
        },
    )
    .unwrap();
    assert_eq!(
        changed_lines(&net),
        [r#"inputs.in = { node = 3, output = "out" }"#]
    );

    let mut net = base.clone();
    net.apply(
        &reg,
        Edit::Move {
            node: NodeId(0),
            pos: [0.1, -7.3],
        },
    )
    .unwrap();
    assert_eq!(changed_lines(&net), ["pos = [0.1, -7.3]"]);
}

#[test]
fn equivalent_hand_written_forms_are_accepted() {
    // Standard tables, keys in any order, no next_id or pos, inline arrays.
    let text = r#"
format = 1
node = [
  { type = "img.noise", name = "a", id = 7 },
]

[[comment]]
id = 2
text = "note"
"#;
    let net = network_from_str(text).unwrap();
    assert_eq!(net.node(NodeId(7)).unwrap().pos, [0.0, 0.0]);
    let canonical = network_to_string(&net).unwrap();
    assert!(canonical.contains("next_id = 8"), "{canonical}");

    let text = r#"
format = 1

[[node]]
id = 0
name = "level1"
type = "img.level"

[node.params]
gamma = 2.0
invert = true

[node.params.brightness]
value = 0.5
"#;
    let net = network_from_str(text).unwrap();
    let node = net.node(NodeId(0)).unwrap();
    assert_eq!(node.params["gamma"].value, json!(2.0));
    assert_eq!(node.params["brightness"].value, json!(0.5));
    assert!(
        network_to_string(&net)
            .unwrap()
            .contains("params.brightness = 0.5")
    );
}

#[test]
fn errors_say_what_and_where() {
    let err = |text: &str| network_from_str(text).unwrap_err().to_string();
    let base = "format = 1\n[[node]]\nid = 0\nname = \"a\"\ntype = \"img.noise\"\n";
    assert!(err(&format!("{base}parms.x = 1\n")).contains("node 0: unknown key `parms`"));
    assert!(err("format = 2\n").contains("newer Atidarla"));
    assert!(err("next_id = 1\n").contains("missing `format`"));
    assert!(
        err(&format!(
            "{base}inputs.in = {{ node = 9, output = \"out\" }}\n"
        ))
        .contains("missing node n9")
    );
    assert!(
        err(&format!("{base}params.z = {{ value = 1.0, bind = 3 }}\n"))
            .contains("parameter `z`: `bind` must be a table")
    );
    assert!(err(&format!("{base}params.z = nan\n")).contains("not a finite number"));
    assert!(err(&format!("{base}params.z = 1979-05-27\n")).contains("dates"));
    assert!(
        err(&format!(
            "{base}[[node]]\nid = 0\nname = \"b\"\ntype = \"img.noise\"\n"
        ))
        .contains("duplicate node")
    );
    assert!(
        err("format = 1\n[[node]\n").contains("line 2"),
        "{}",
        err("format = 1\n[[node]\n")
    );
    // Schema problems surface when validated against a registry.
    let net = network_from_str(&format!("{base}params.nope = 1\n")).unwrap();
    assert!(net.validated(&registry()).is_err());
}

/// Damaged files (cut, duplicated ranges, changed bytes) are errors, never
/// panics; whatever does parse round-trips.
#[test]
fn mutated_files_never_panic() {
    let reg = registry();
    let mut rng = Rng(0xfeed_beef_1234_5678);
    let seeds: Vec<String> = (0..8)
        .map(|_| network_to_string(&random_network(&reg, &mut rng)).unwrap())
        .chain([SAMPLE.to_owned()])
        .collect();
    let rounds: usize = std::env::var("ATD_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_000);
    let mut parsed = 0;
    for _ in 0..rounds {
        let mut bytes = seeds[rng.next(seeds.len() as u64) as usize]
            .clone()
            .into_bytes();
        let len = bytes.len() as u64;
        match rng.next(4) {
            0 => {
                for _ in 0..1 + rng.next(4) {
                    let i = rng.next(len) as usize;
                    let set = b"0123456789=[]{}\"'.,\n -_abcz#";
                    bytes[i] = set[rng.next(set.len() as u64) as usize];
                }
            }
            1 => bytes.truncate(rng.next(len) as usize),
            2 => {
                let a = rng.next(len) as usize;
                let b = (a + rng.next(80) as usize).min(bytes.len());
                bytes.drain(a..b);
            }
            _ => {
                let a = rng.next(len) as usize;
                let b = (a + rng.next(80) as usize).min(bytes.len());
                let chunk = bytes[a..b].to_vec();
                let at = rng.next(len) as usize;
                bytes.splice(at..at, chunk);
            }
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if let Ok(net) = network_from_str(&text) {
            parsed += 1;
            let again = network_to_string(&net).unwrap();
            assert_eq!(network_from_str(&again).unwrap(), net);
            let _ = net.validated(&reg);
        }
    }
    assert!(parsed > 0);
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atd-format-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn project_folders_save_and_load() {
    let reg = registry();
    let dir = temp_dir("save");
    let mut rng = Rng(42);
    let mut project = Project::with_main("Show ✨", random_network(&reg, &mut rng));
    project.description = "Line one.\nLine two.".into();
    project.resolution = [1920, 1080];
    project
        .networks
        .insert("extra".into(), random_network(&reg, &mut rng));
    save(&dir, &project).unwrap();
    assert_eq!(load(&dir).unwrap(), project);
    let snapshot = |dir: &Path| {
        [
            "project.toml",
            "networks/main.toml",
            "networks/extra.toml",
            ".gitignore",
        ]
        .map(|f| std::fs::read(dir.join(f)).unwrap())
    };
    let first = snapshot(&dir);
    save(&dir, &load(&dir).unwrap()).unwrap();
    assert_eq!(snapshot(&dir), first, "saving again writes the same bytes");
    assert_eq!(first[3], b".cache/\n");

    // A removed network's file goes; the user's own files stay.
    std::fs::write(dir.join("notes.txt"), "mine").unwrap();
    std::fs::write(dir.join(".gitignore"), "custom\n").unwrap();
    project.networks.remove("extra");
    save(&dir, &project).unwrap();
    assert!(!dir.join("networks/extra.toml").exists());
    assert!(dir.join("notes.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "custom\n"
    );
    assert_eq!(load(&dir).unwrap(), project);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn project_errors_name_the_file() {
    let dir = temp_dir("errors");
    assert!(matches!(load(&dir), Err(FormatError::Io { .. })));
    save(&dir, &Project::new("x")).unwrap();
    std::fs::write(dir.join("networks/main.toml"), "format = 1\nbad = 2\n").unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.starts_with("networks/main.toml: "), "{e}");
    std::fs::remove_file(dir.join("networks/main.toml")).unwrap();
    assert!(
        load(&dir)
            .unwrap_err()
            .to_string()
            .contains("no networks/main.toml")
    );
    std::fs::write(
        dir.join("project.toml"),
        "format = 1\nname = \"x\"\nresolution = [0, 5]\n",
    )
    .unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.starts_with("project.toml: `resolution`"), "{e}");
    let mut p = Project::new("x");
    p.networks.insert("Bad Name".into(), Network::default());
    assert!(save(&dir, &p).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A network saved before the beat clock grew tap, sources and pulses
/// (M3 FR-S4): it still loads, validates, keeps the parameters it had and
/// saves back unchanged (rule 8).
const OLD_BEAT: &str = r#"# Atidarla network. Node IDs are stable: inputs and bindings refer to them.
format = 1
next_id = 2

[[node]]
id = 0
name = "clock"
type = "sig.beat"
pos = [0.0, 0.0]
params.beats_per_bar = 3
params.bpm = 90.0

[[node]]
id = 1
name = "bar_phase"
type = "sig.select"
pos = [220.0, 0.0]
inputs.in = { node = 0, output = "out" }
params.channels = "bar_phase"
"#;

#[test]
fn a_network_saved_before_the_beat_clock_grew_still_loads() {
    let reg = registry();
    let net = network_from_str(OLD_BEAT).unwrap().validated(&reg).unwrap();
    let clock = net.node_by_name("clock").unwrap();
    assert_eq!(clock.params["bpm"].value, json!(90.0));
    assert_eq!(clock.params["beats_per_bar"].value, json!(3));
    assert!(!clock.params.contains_key("source"));
    // The new parameters take their defaults: the internal clock.
    assert_eq!(
        net.param_value(&reg, clock.id, "source"),
        Some(&json!("internal"))
    );
    assert_eq!(network_to_string(&net).unwrap(), OLD_BEAT);
}
