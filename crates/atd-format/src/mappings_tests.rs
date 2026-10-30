use super::mappings::*;
use super::*;
use atd_core::graph::NodeId;

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
    fn float(&mut self) -> f64 {
        (self.next(20_001) as f64 - 10_000.0) / 37.0
    }
}

const NAMES: [&str; 5] = [
    "",
    "nanoKONTROL2",
    "a \"quoted\" name",
    "émoji ✨",
    "line\nbreak",
];

fn random_mappings(rng: &mut Rng) -> Mappings {
    let mut m = Mappings::default();
    if rng.next(2) == 0 {
        m.osc = OscSettings {
            port: rng.next(65_536) as u16,
            oscquery: rng.next(2) == 0,
            oscquery_port: rng.next(65_536) as u16,
        };
    }
    for _ in 0..rng.next(12) {
        let source = if rng.next(3) == 0 {
            Source::Osc {
                address: format!("/a/{}", rng.next(100)),
                arg: rng.next(4) as usize,
            }
        } else {
            Source::Midi {
                control: match rng.next(3) {
                    0 => MidiControl::Cc(rng.next(128) as u8),
                    1 => MidiControl::Note(rng.next(128) as u8),
                    _ => MidiControl::Bend,
                },
                channel: (rng.next(2) == 0).then(|| 1 + rng.next(16) as u8),
                device: NAMES[rng.next(NAMES.len() as u64) as usize].to_owned(),
            }
        };
        let target = Target {
            node: NodeId(rng.next(1000)),
            param: ["colour", "gamma", "x y", "rotation"][rng.next(4) as usize].to_owned(),
            component: (rng.next(2) == 0).then(|| rng.next(4) as usize),
        };
        let id = m.add(source, target, [rng.float(), rng.float()]);
        let x = m.mappings.iter_mut().find(|x| x.id == id).unwrap();
        x.input = [rng.float(), rng.float()];
        x.curve = match rng.next(3) {
            0 => Curve::Linear,
            1 => Curve::Exp(rng.float()),
            _ => Curve::Step(2 + rng.next(30) as u32),
        };
        x.mode = Mode::ALL[rng.next(4) as usize];
        x.pickup = rng.next(2) == 0;
    }
    // Deleting leaves gaps; IDs are never reused.
    if m.mappings.len() > 2 && rng.next(2) == 0 {
        let id = m.mappings[1].id;
        m.remove(id);
    }
    m
}

#[test]
fn random_mappings_round_trip_exactly_and_byte_stable() {
    let mut rng = Rng(0x5eed_1234_abcd);
    for _ in 0..300 {
        let m = random_mappings(&mut rng);
        let text = mappings_to_string(&m).unwrap();
        let back = mappings_from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(back, m, "{text}");
        assert_eq!(mappings_to_string(&back).unwrap(), text);
    }
}

#[test]
fn the_canonical_form_is_stable() {
    let mut m = Mappings::default();
    m.add(
        Source::Midi {
            control: MidiControl::Cc(7),
            channel: Some(1),
            device: "nanoKONTROL".into(),
        },
        Target {
            node: NodeId(3),
            param: "colour".into(),
            component: Some(0),
        },
        [0.0, 1.0],
    );
    let id = m.add(
        Source::Osc {
            address: "/fader/1".into(),
            arg: 0,
        },
        Target {
            node: NodeId(4),
            param: "gamma".into(),
            component: None,
        },
        [0.5, 2.0],
    );
    let x = m.mappings.iter_mut().find(|x| x.id == id).unwrap();
    x.curve = Curve::Exp(2.0);
    x.mode = Mode::Relative;
    x.pickup = true;
    let text = mappings_to_string(&m).unwrap();
    let expected = "\
# Atidarla mappings: MIDI and OSC controls to parameters (docs/mappings.md).
# Targets refer to node IDs in networks/main.toml.
format = 1
next_id = 2

[[mapping]]
id = 0
source = { midi = \"cc\", number = 7, channel = 1, device = \"nanoKONTROL\" }
target = { node = 3, param = \"colour\", component = 0 }
input = [0.0, 127.0]
output = [0.0, 1.0]
curve = \"linear\"
mode = \"absolute\"
pickup = false

[[mapping]]
id = 1
source = { osc = \"/fader/1\", arg = 0 }
target = { node = 4, param = \"gamma\" }
input = [0.0, 1.0]
output = [0.5, 2.0]
curve = { exp = 2.0 }
mode = \"relative\"
pickup = true
";
    assert_eq!(text, expected);
}

#[test]
fn small_edits_are_small_diffs() {
    let mut rng = Rng(77);
    let mut m = random_mappings(&mut rng);
    while m.mappings.len() < 4 {
        m = random_mappings(&mut rng);
    }
    let before = mappings_to_string(&m).unwrap();
    m.mappings[2].pickup = !m.mappings[2].pickup;
    let after = mappings_to_string(&m).unwrap();
    let changed = before
        .lines()
        .zip(after.lines())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(before.lines().count(), after.lines().count());
    assert_eq!(changed, 1, "{before}\n---\n{after}");
}

#[test]
fn hand_written_files_get_defaults() {
    let m = mappings_from_str(
        r#"
format = 1
[[mapping]]
id = 4
source = { midi = "bend" }
target = { node = 1, param = "x" }

[[mapping]]
id = 2
source.osc = "/x"
target.node = 2
target.param = "y"
curve = { step = 3 }
mode = "toggle"
"#,
    )
    .unwrap();
    assert_eq!(m.next_id, 5);
    assert_eq!(m.mappings[0].id, 2, "sorted by ID");
    assert_eq!(m.mappings[0].input, [0.0, 1.0]);
    assert_eq!(m.mappings[0].curve, Curve::Step(3));
    assert_eq!(m.mappings[0].mode, Mode::Toggle);
    let bend = &m.mappings[1];
    assert_eq!(bend.input, [-8192.0, 8191.0]);
    assert_eq!(bend.output, [0.0, 1.0]);
    assert_eq!(bend.curve, Curve::Linear);
    assert_eq!(bend.mode, Mode::Absolute);
    assert!(!bend.pickup);
    assert_eq!(
        bend.source,
        Source::Midi {
            control: MidiControl::Bend,
            channel: None,
            device: String::new()
        }
    );
}

#[test]
fn errors_say_what_and_where() {
    let cases = [
        ("format = 1\nnope = 1", "unknown key `nope`"),
        ("format = 2", "newer Atidarla"),
        (
            "format = 1\n[[mapping]]\nid = 0\nsource = { midi = \"cc\" }\ntarget = { node = 1, param = \"x\" }",
            "mapping 0, source: `number` (0–127) is required",
        ),
        (
            "format = 1\n[[mapping]]\nid = 0\nsource = { midi = \"cc\", number = 1, channel = 0 }\ntarget = { node = 1, param = \"x\" }",
            "`channel` is 1–16",
        ),
        (
            "format = 1\n[[mapping]]\nid = 0\nsource = { osc = \"x\" }\ntarget = { node = 1, param = \"x\" }",
            "starts with `/`",
        ),
        (
            "format = 1\n[[mapping]]\nid = 0\nsource = { osc = \"/x\" }\ntarget = { node = 1, param = \"x\" }\ncurve = { step = 1 }",
            "`curve` is",
        ),
        (
            "format = 1\n[[mapping]]\nid = 0\nsource = { osc = \"/x\" }\ntarget = { node = 1, param = \"x\" }\nmode = \"latch\"",
            "`mode` is absolute",
        ),
        (
            "format = 1\n[[mapping]]\nid = 0\nsource = { osc = \"/x\" }\ntarget = { node = 1, param = \"x\" }\n[[mapping]]\nid = 0\nsource = { osc = \"/y\" }\ntarget = { node = 1, param = \"x\" }",
            "mapping 0: the ID is used twice",
        ),
        ("format = 1\n[osc]\nport = 70000", "port number"),
    ];
    for (text, want) in cases {
        let e = mappings_from_str(text).unwrap_err().to_string();
        assert!(e.contains(want), "{text:?}: {e}");
    }
}

#[test]
fn mutated_files_never_panic() {
    let mut rng = Rng(9);
    let mut m = random_mappings(&mut rng);
    while m.mappings.is_empty() {
        m = random_mappings(&mut rng);
    }
    let text = mappings_to_string(&m).unwrap();
    let bytes = text.as_bytes();
    for i in 0..2000 {
        let mut b = bytes.to_vec();
        let at = rng.next(b.len() as u64) as usize;
        match i % 3 {
            0 => b[at] = b"=[]{}\"0x-.,\n"[rng.next(12) as usize],
            1 => {
                b.remove(at);
            }
            _ => b.truncate(at),
        }
        if let Ok(s) = std::str::from_utf8(&b) {
            let _ = mappings_from_str(s);
        }
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atd-mappings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn projects_keep_their_mappings_and_old_projects_load_unchanged() {
    let dir = temp_dir("project");
    let mut p = Project::new("Mapped");
    save(&dir, &p).unwrap();
    assert!(
        !dir.join(mappings::FILE).exists(),
        "no mappings, no file: old projects stay as they were"
    );
    assert_eq!(load(&dir).unwrap().mappings, Mappings::default());

    p.mappings.add(
        Source::Osc {
            address: "/level".into(),
            arg: 1,
        },
        Target {
            node: NodeId(0),
            param: "gamma".into(),
            component: None,
        },
        [0.0, 2.0],
    );
    p.mappings.osc.oscquery = true;
    save(&dir, &p).unwrap();
    let back = load(&dir).unwrap();
    assert_eq!(back.mappings, p.mappings);

    // Removing every mapping (and OSC settings) removes the file.
    p.mappings = Mappings::default();
    save(&dir, &p).unwrap();
    assert!(!dir.join(mappings::FILE).exists());

    std::fs::write(dir.join(mappings::FILE), "format = 1\nbad = 1\n").unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.starts_with("mappings.toml: "), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}
