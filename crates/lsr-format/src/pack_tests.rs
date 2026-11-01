use super::*;
use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::pack::{
    PackDef, PackInput, PackOutput, PackSet, ParamTarget, PromotedParam, Version, VersionReq,
};
use lsr_core::registry::Registry;
use serde_json::json;

fn registry() -> Registry {
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    reg
}

const SAMPLE: &str = r#"# Lasair Pack. Its network is network.toml; its help page is help.md.
format = 1
name = "glow"
version = "1.2.0"
label = "Glow"
summary = "A soft glow around bright areas."
locked = true

[[param]]
name = "amount"
label = "Amount"
default = 0.8
min = 0.0
max = 1.0
help = "How strong the glow is."
to = [{ node = 1, param = "opacity" }]

[[param]]
name = "size"
default = 12.0
to = [{ node = 0, param = "size" }]

[[input]]
name = "in"
to = [{ node = 0, input = "in" }, { node = 1, input = "a" }]

[[output]]
name = "out"
from = { node = 1, output = "out" }
"#;

const NETWORK: &str = r#"# Lasair network. Node IDs are stable: inputs and bindings refer to them.
format = 1
next_id = 2

[[node]]
id = 0
name = "blur1"
type = "img.blur"
pos = [0.0, 0.0]

[[node]]
id = 1
name = "add1"
type = "img.composite"
pos = [220.0, 0.0]
inputs.b = { node = 0, output = "out" }
params.operation = "add"
"#;

fn sample() -> PackDef {
    let mut def = pack_from_str(SAMPLE).unwrap();
    def.network = network_from_str(NETWORK).unwrap();
    def.help = "# Glow\n\nBlurs the input and adds it back.\n".into();
    def
}

#[test]
fn the_canonical_pack_form_is_stable_and_valid() {
    let def = sample();
    assert_eq!(def.version, Version::new(1, 2, 0));
    assert!(def.locked);
    assert_eq!(def.inputs[0].targets.len(), 2);
    assert_eq!(pack_to_string(&def).unwrap(), SAMPLE);
    let packs = {
        let mut s = PackSet::default();
        s.insert(def);
        s
    };
    let reg = packs.register(&registry()).unwrap();
    let schema = reg.get("cmp.pack_glow").unwrap();
    assert_eq!(schema.params.len(), 2);
    assert_eq!(
        schema.params[1].label, "Size",
        "label from the inner parameter"
    );
}

#[test]
fn pack_edits_are_one_line_diffs() {
    let base = sample();
    let before = pack_to_string(&base).unwrap();
    let changed = |def: &PackDef| {
        let after = pack_to_string(def).unwrap();
        let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(a.len(), b.len(), "same number of lines:\n{after}");
        a.iter()
            .zip(&b)
            .filter(|(x, y)| x != y)
            .map(|(_, y)| y.to_string())
            .collect::<Vec<_>>()
    };
    let mut d = base.clone();
    d.params[0].default = json!(1.0);
    assert_eq!(changed(&d), ["default = 1.0"]);
    let mut d = base.clone();
    d.params[0].max = Some(4.0);
    assert_eq!(changed(&d), ["max = 4.0"]);
    let mut d = base.clone();
    d.version = Version::new(1, 2, 1);
    assert_eq!(changed(&d), [r#"version = "1.2.1""#]);
    let mut d = base.clone();
    d.locked = false;
    assert_eq!(changed(&d), ["locked = false"]);
    let mut d = base;
    d.params[1]
        .targets
        .push(ParamTarget::new(NodeId(1), "opacity"));
    d.params[0].targets.clear();
    d.params[0]
        .targets
        .push(ParamTarget::new(NodeId(1), "operation"));
    assert_eq!(
        changed(&d),
        [
            r#"to = [{ node = 1, param = "operation" }]"#,
            r#"to = [{ node = 0, param = "size" }, { node = 1, param = "opacity" }]"#
        ]
    );
}

#[test]
fn hand_written_equivalents_are_accepted() {
    let text = r#"
format = 1
name = "glow"
version = "1.0.0"

[[param]]
name = "size"
default = 4
min = 0
to = { node = 0, param = "size" }

[[output]]
name = "out"

[output.from]
node = 0
output = "out"
"#;
    let d = pack_from_str(text).unwrap();
    assert!(!d.locked, "locked defaults to false");
    assert_eq!(d.params[0].min, Some(0.0));
    assert_eq!(d.params[0].targets.len(), 1);
    let canonical = pack_to_string(&d).unwrap();
    assert!(canonical.contains("min = 0.0\n"), "{canonical}");
    assert_eq!(pack_from_str(&canonical).unwrap(), d);
}

#[test]
fn pack_errors_say_what_and_where() {
    let err = |text: &str| pack_from_str(text).unwrap_err().to_string();
    let head = "format = 1\nname = \"glow\"\nversion = \"1.0.0\"\n";
    assert!(err("format = 1\nname = \"glow\"\n").contains("`version` must be a string"));
    assert!(
        err("format = 1\nname = \"glow\"\nversion = \"1.0\"\n").contains("invalid version `1.0`")
    );
    assert!(
        err("format = 1\nname = \"Glow\"\nversion = \"1.0.0\"\n")
            .contains("invalid Pack name `Glow`")
    );
    assert!(err(&format!("{head}lockd = true\n")).contains("unknown key `lockd`"));
    assert!(err(&format!("{head}locked = 1\n")).contains("true or false"));
    assert!(err(&format!("{head}derived_from = \"glow\"\n")).contains("name@1.2.3"));
    assert!(
        err(&format!("{head}[[param]]\nname = \"a\"\nto = []\n"))
            .contains("parameter `a`: missing `default`")
    );
    assert!(
        err(&format!(
            "{head}[[param]]\nname = \"a\"\ndefault = 1\nmin = \"x\"\nto = []\n"
        ))
        .contains("`min` must be a number")
    );
    assert!(
        err(&format!(
            "{head}[[param]]\nname = \"a\"\ndefault = 1\nto = 3\n"
        ))
        .contains("`to` must be a list")
    );
    assert!(
        err(&format!(
            "{head}[[input]]\nname = \"in\"\nto = [{{ node = 0, socket = \"in\" }}]\n"
        ))
        .contains("input `in`: unknown key `socket`")
    );
    assert!(
        err(&format!("{head}[[output]]\nname = \"out\"\n"))
            .contains("output `out`: missing `from`")
    );
    assert!(err("format = 9\nname = \"glow\"\nversion = \"1.0.0\"\n").contains("newer Lasair"));
}

/// A random, valid Pack over the real operators.
fn random_pack(reg: &Registry, seed: u64) -> PackDef {
    let mut state = seed;
    let mut rng = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n.max(1)
    };
    let types: Vec<&str> = reg
        .iter()
        .filter(|s| !s.params.is_empty() && !s.outputs.is_empty())
        .map(|s| s.type_id.as_str())
        .collect();
    let mut net = lsr_core::graph::Network::default();
    let count = 1 + rng(6);
    for i in 0..count {
        let id = net.alloc_id();
        let ty = types[rng(types.len() as u64) as usize];
        let mut node = Node::new(id, ty, format!("n{i}"));
        node.pos = [rng(500) as f32 / 4.0, 0.5];
        net.apply(reg, Edit::AddNode { node }).unwrap();
        if i > 0 {
            let from = NodeId(rng(i));
            let from_out = reg.get(&net.node(from).unwrap().type_id).unwrap().outputs[0]
                .name
                .clone();
            if let Some(input) = reg.get(ty).unwrap().inputs.first() {
                let _ = net.apply(
                    reg,
                    Edit::Connect {
                        wire: Wire::new(
                            Endpoint::new(from, from_out),
                            Endpoint::new(id, &input.name),
                        ),
                    },
                );
            }
        }
    }
    let mut params = Vec::new();
    for node in net.nodes() {
        let schema = reg.get(&node.type_id).unwrap();
        let p = &schema.params[rng(schema.params.len() as u64) as usize];
        if rng(2) == 0 {
            continue;
        }
        params.push(PromotedParam {
            name: format!("p{}_{}", node.id.0, p.name),
            label: ["", "Label ✨", "has \"quotes\"\nand lines"][rng(3) as usize].into(),
            default: p.default.clone(),
            min: None,
            max: None,
            help: ["", "Help.", "Two\nlines."][rng(3) as usize].into(),
            targets: vec![ParamTarget::new(node.id, &p.name)],
        });
    }
    let mut inputs = Vec::new();
    for node in net.nodes() {
        let schema = reg.get(&node.type_id).unwrap();
        for (k, s) in schema.inputs.iter().enumerate() {
            if net.inputs_of(node.id).any(|(name, _)| name == s.name) || rng(2) == 0 {
                continue;
            }
            inputs.push(PackInput {
                name: format!("in{}_{k}", node.id.0),
                targets: vec![Endpoint::new(node.id, &s.name)],
            });
        }
    }
    let last = net.nodes().last().unwrap();
    let out = reg.get(&last.type_id).unwrap().outputs[0].name.clone();
    PackDef {
        version: Version::new(rng(3) as u32, rng(20) as u32, rng(5) as u32),
        label: ["", "Glow"][rng(2) as usize].into(),
        summary: ["", "Sum'''mary"][rng(2) as usize].into(),
        help: ["", "# Help\n\nText.\n"][rng(2) as usize].into(),
        locked: rng(2) == 0,
        derived_from: (rng(3) == 0).then(|| "base@1.0.0".parse().unwrap()),
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(last.id, out),
        }],
        params,
        inputs,
        network: net,
        ..PackDef::new(format!("pack{}", seed % 1000))
    }
}

#[test]
fn random_packs_round_trip_exactly_and_byte_stable() {
    let reg = registry();
    for seed in 1..200u64 {
        let def = random_pack(&reg, seed * 0x9E37_79B9);
        let mut set = PackSet::default();
        set.insert(def.clone());
        set.register(&reg)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}\n{}", pack_to_string(&def).unwrap()));
        let text = pack_to_string(&def).unwrap();
        let mut back = pack_from_str(&text).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{text}"));
        back.network = def.network.clone();
        back.help = def.help.clone();
        assert_eq!(back, def, "seed {seed}:\n{text}");
        assert_eq!(pack_to_string(&back).unwrap(), text);
    }
}

/// Damaged `pack.toml` files are errors, never panics; what parses
/// round-trips, and registering it never panics either.
#[test]
fn mutated_pack_files_never_panic() {
    let reg = registry();
    let seeds: Vec<String> = (1..6)
        .map(|s| pack_to_string(&random_pack(&reg, s * 7919)).unwrap())
        .chain([SAMPLE.to_owned()])
        .collect();
    let net = network_from_str(NETWORK).unwrap();
    let mut state = 0xdead_beef_cafe_f00du64;
    let mut rng = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n.max(1)
    };
    let rounds: usize = std::env::var("LSR_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_000);
    let mut parsed = 0;
    for _ in 0..rounds {
        let mut bytes = seeds[rng(seeds.len() as u64) as usize].clone().into_bytes();
        let len = bytes.len() as u64;
        match rng(4) {
            0 => {
                for _ in 0..1 + rng(4) {
                    let i = rng(len) as usize;
                    let set = b"0123456789=[]{}\"'.,\n -_abcz#@^";
                    bytes[i] = set[rng(set.len() as u64) as usize];
                }
            }
            1 => bytes.truncate(rng(len) as usize),
            2 => {
                let a = rng(len) as usize;
                let b = (a + rng(80) as usize).min(bytes.len());
                bytes.drain(a..b);
            }
            _ => {
                let a = rng(len) as usize;
                let b = (a + rng(80) as usize).min(bytes.len());
                let chunk = bytes[a..b].to_vec();
                let at = rng(len) as usize;
                bytes.splice(at..at, chunk);
            }
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if let Ok(mut def) = pack_from_str(&text) {
            parsed += 1;
            let again = pack_to_string(&def).unwrap();
            assert_eq!(pack_from_str(&again).unwrap(), def);
            def.network = net.clone();
            let mut set = PackSet::default();
            set.insert(def);
            if let Ok(r) = set.register(&reg) {
                let mut top = lsr_core::graph::Network::default();
                for d in set.iter() {
                    let id = top.alloc_id();
                    let _ = top.apply(
                        &r,
                        Edit::AddNode {
                            node: Node::new(id, d.type_id(), "x"),
                        },
                    );
                }
                let _ = set.flatten(&top, &r);
            }
        }
    }
    assert!(parsed > 0);
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lsr-format-pack-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn projects_save_and_load_their_packs() {
    let reg = registry();
    let dir = temp_dir("project");
    let mut project = Project::new("With packs");
    project.packs.insert(sample());
    let mut other = random_pack(&reg, 5);
    other.name = "other".into();
    other.help.clear();
    project.packs.insert(other);
    let packs = project.packs.register(&reg).unwrap();
    let main = project.networks.get_mut(MAIN).unwrap();
    let id = main.alloc_id();
    main.apply(
        &packs,
        Edit::AddNode {
            node: Node::new(id, "cmp.pack_glow", "glow1"),
        },
    )
    .unwrap();
    main.apply(
        &packs,
        Edit::SetParam {
            node: id,
            param: "amount".into(),
            value: Some(Param::constant(json!(0.5))),
        },
    )
    .unwrap();
    save(&dir, &project).unwrap();
    assert_eq!(load(&dir).unwrap(), project);
    for f in [
        "packs/glow/pack.toml",
        "packs/glow/network.toml",
        "packs/glow/help.md",
        "packs/other/pack.toml",
    ] {
        assert!(dir.join(f).exists(), "{f}");
    }
    assert!(
        !dir.join("packs/other/help.md").exists(),
        "no help, no help.md"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("packs/glow/pack.toml")).unwrap(),
        SAMPLE
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("packs/glow/network.toml")).unwrap(),
        NETWORK
    );
    let main_text = std::fs::read_to_string(dir.join("networks/main.toml")).unwrap();
    assert!(
        main_text.contains("type = \"cmp.pack_glow\"\n")
            && main_text.contains("params.amount = 0.5\n"),
        "{main_text}"
    );
    let snapshot = |dir: &Path| {
        [
            "packs/glow/pack.toml",
            "packs/glow/network.toml",
            "packs/glow/help.md",
            "packs/other/pack.toml",
        ]
        .map(|f| std::fs::read(dir.join(f)).unwrap())
    };
    let first = snapshot(&dir);
    save(&dir, &load(&dir).unwrap()).unwrap();
    assert_eq!(snapshot(&dir), first, "saving again writes the same bytes");

    // A removed Pack's files go; a file of the user's in its folder stays.
    std::fs::write(dir.join("packs/other/notes.txt"), "mine").unwrap();
    project.packs.remove("other");
    project.packs.get("glow").cloned().map(|mut g| {
        g.help.clear();
        project.packs.insert(g)
    });
    save(&dir, &project).unwrap();
    assert!(!dir.join("packs/other/pack.toml").exists());
    assert!(dir.join("packs/other/notes.txt").exists());
    assert!(!dir.join("packs/glow/help.md").exists(), "help removed");
    assert_eq!(
        load(&dir).unwrap(),
        project,
        "a folder without pack.toml is skipped"
    );
    std::fs::remove_file(dir.join("packs/other/notes.txt")).unwrap();
    save(&dir, &project).unwrap();
    assert!(
        !dir.join("packs/other").exists(),
        "an empty stale folder goes"
    );
    assert_eq!(load(&dir).unwrap(), project);

    // Errors name the file.
    std::fs::write(
        dir.join("packs/glow/pack.toml"),
        SAMPLE.replace("name = \"glow\"", "name = \"glo\""),
    )
    .unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(
        e.starts_with("packs/glow/pack.toml: Pack `glo` must be in a folder named `glo`"),
        "{e}"
    );
    std::fs::write(dir.join("packs/glow/pack.toml"), "format = 1\nname = 3\n").unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.starts_with("packs/glow/pack.toml: "), "{e}");
    std::fs::write(dir.join("packs/glow/pack.toml"), SAMPLE).unwrap();
    std::fs::write(
        dir.join("packs/glow/network.toml"),
        "format = 1\nnode = 3\n",
    )
    .unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.starts_with("packs/glow/network.toml: "), "{e}");
    std::fs::remove_file(dir.join("packs/glow/network.toml")).unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.contains("packs/glow/network.toml: missing"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn libraries_find_packs_by_name_and_version() {
    let dir = temp_dir("library");
    let mut v1 = sample();
    v1.version = Version::new(1, 0, 0);
    let mut v12 = sample();
    v12.version = Version::new(1, 2, 0);
    let mut v2 = sample();
    v2.version = Version::new(2, 0, 0);
    save_pack(&dir.join("glow@1.0.0"), &v1).unwrap();
    save_pack(&dir.join("glow@1.2.0"), &v12).unwrap();
    save_pack(&dir.join("glow"), &v2).unwrap();
    std::fs::create_dir_all(dir.join("not-a-pack")).unwrap();
    std::fs::write(dir.join("README.md"), "Packs").unwrap();
    let lib = Library::open(&dir).unwrap();
    assert_eq!(lib.len(), 3);
    let find = |req: &str| {
        lib.find("glow", req.parse::<VersionReq>().unwrap())
            .map(|d| d.version)
    };
    assert_eq!(find("*").unwrap(), Version::new(2, 0, 0));
    assert_eq!(find("^1").unwrap(), Version::new(1, 2, 0));
    assert_eq!(find("1.0.0").unwrap(), Version::new(1, 0, 0));
    let e = find("^3").unwrap_err().to_string();
    assert_eq!(
        e,
        "Pack `glow` ^3.0.0 is not in the library (it has 1.0.0, 1.2.0, 2.0.0)"
    );
    assert!(
        lib.find("nope", VersionReq::Any)
            .unwrap_err()
            .to_string()
            .contains("no Pack `nope`")
    );
    assert_eq!(
        lib.find_spec("glow@^1.0").unwrap().version,
        Version::new(1, 2, 0)
    );
    assert_eq!(lib.latest().count(), 1);
    assert!(
        lib.path("glow", Version::new(1, 0, 0))
            .unwrap()
            .ends_with("glow@1.0.0")
    );

    // A folder named after another Pack or version is an error.
    save_pack(&dir.join("glow@9.9.9"), &v1).unwrap();
    let e = Library::open(&dir).unwrap_err().to_string();
    assert!(
        e.contains("packs/glow@9.9.9/pack.toml") && e.contains("must be named"),
        "{e}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
