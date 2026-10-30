use super::timeline::{Cue, CueAction, Interp, Key, Marker, Track};
use super::*;
use atd_core::graph::NodeId;
use serde_json::json;

const SAMPLE: &str = r#"# Atidarla timeline. Times are in seconds; tracks and cues refer to node IDs.
format = 1
length = 32.0
fps = 60.0
loop = [8.0, 16.0]

[[track]]
node = 3
param = "rotation"
keys = [
    { t = 0.0, v = 0.0 },
    { t = 4.0, v = 90.0, interp = "bezier", out = [1.0, 0.0] },
    { t = 8.0, v = 180.0, interp = "step", in = [-1.0, 0.0] },
]

[[track]]
node = 5
param = "color"
component = 2
keys = [
    { t = 0.0, v = 1.0 },
]

[[marker]]
id = 1
t = 8.0
name = "Drop"

[[cue]]
id = 2
name = "Chorus"
t = 16.0
when = "op('beat1')['beat'] > 0"
actions = [
    { launch = 1, layers = [7, 8] },
    { node = 4, param = "opacity", live = 0.5 },
    { node = 4, param = "blend", set = "add" },
]

[[cue]]
id = 3
name = "Blackout"
actions = [
    { node = 4, param = "opacity", set = 0.0 },
]
"#;

#[test]
fn sample_is_canonical_and_reads_as_written() {
    let doc = timeline_from_str(SAMPLE).unwrap();
    assert_eq!(doc.length, 32.0);
    assert_eq!(doc.loop_region, Some([8.0, 16.0]));
    assert_eq!(doc.tracks.len(), 2);
    let rot = doc.track(NodeId(3), "rotation", None).unwrap();
    assert_eq!(rot.keys[1].interp, Interp::Bezier);
    assert_eq!(rot.keys[1].tan_out, Some([1.0, 0.0]));
    assert_eq!(rot.keys[2].tan_in, Some([-1.0, 0.0]));
    assert_eq!(doc.cues[0].when.as_deref(), Some("op('beat1')['beat'] > 0"));
    assert_eq!(
        doc.cues[0].actions[0],
        CueAction::Launch {
            layers: vec![NodeId(7), NodeId(8)],
            slot: 1
        }
    );
    assert_eq!(doc.cues[1].t, None);
    assert_eq!(timeline_to_string(&doc).unwrap(), SAMPLE);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
    fn time(&mut self) -> f64 {
        self.next(100_000) as f64 / 997.0
    }
}

fn random_timeline(rng: &mut Rng) -> TimelineDoc {
    let mut doc = TimelineDoc {
        length: 1.0 + rng.time(),
        fps: [24.0, 25.0, 29.97, 30.0, 60.0][rng.next(5) as usize],
        loop_region: (rng.next(2) == 0).then_some([1.0, 2.5]),
        ..Default::default()
    };
    for _ in 0..rng.next(6) {
        let comp = (rng.next(2) == 0).then(|| rng.next(4) as usize);
        let node = NodeId(rng.next(20));
        let param = ["scale", "rotation", "color", "z"][rng.next(4) as usize];
        let track = doc.track_mut(node, param, comp);
        for _ in 0..rng.next(8) {
            let t = rng.time();
            let i = track.set_key(t, rng.time() - 50.0);
            track.keys[i].interp = Interp::ALL[rng.next(3) as usize];
            if rng.next(3) == 0 {
                track.keys[i].tan_out = Some([rng.time() / 100.0, rng.time() - 30.0]);
            }
            if rng.next(3) == 0 {
                track.keys[i].tan_in = Some([-rng.time() / 100.0, 1.5]);
            }
        }
    }
    for _ in 0..rng.next(4) {
        let id = doc.next_id();
        doc.markers.push(Marker {
            id,
            t: rng.time(),
            name: ["", "Drop", "a \"quoted\" name", "émoji ✨"][rng.next(4) as usize].into(),
        });
    }
    for _ in 0..rng.next(4) {
        let id = doc.next_id();
        let mut actions = Vec::new();
        for _ in 0..rng.next(4) {
            actions.push(match rng.next(3) {
                0 => CueAction::Set {
                    node: NodeId(rng.next(9)),
                    param: "mode".into(),
                    value: [json!("add"), json!(2), json!(true), json!([0.5, 1.0])]
                        [rng.next(4) as usize]
                        .clone(),
                },
                1 => CueAction::Live {
                    node: NodeId(rng.next(9)),
                    param: "opacity".into(),
                    value: json!(rng.time()),
                },
                _ => CueAction::Launch {
                    layers: (0..rng.next(3)).map(NodeId).collect(),
                    slot: rng.next(16) as i64 - 1,
                },
            });
        }
        doc.cues.push(Cue {
            id,
            name: format!("Cue {id}"),
            t: (rng.next(3) != 0).then(|| rng.time()),
            when: (rng.next(3) == 0).then(|| "op('beat1')['beat'] > 0.5".to_owned()),
            actions,
        });
    }
    doc
}

#[test]
fn random_timelines_round_trip_exactly() {
    let mut rng = Rng(0x7157_1e11);
    for _ in 0..300 {
        let mut doc = random_timeline(&mut rng);
        let text = timeline_to_string(&doc).unwrap();
        let back = timeline_from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        doc.canonicalize();
        assert_eq!(back, doc, "{text}");
        assert_eq!(timeline_to_string(&back).unwrap(), text);
    }
}

#[test]
fn order_in_memory_does_not_change_the_bytes() {
    let mut doc = timeline_from_str(SAMPLE).unwrap();
    doc.tracks.reverse();
    doc.cues.reverse();
    for t in &mut doc.tracks {
        t.keys.reverse();
    }
    assert_eq!(timeline_to_string(&doc).unwrap(), SAMPLE);
}

#[test]
fn small_edits_are_one_line_diffs() {
    let base = timeline_from_str(SAMPLE).unwrap();
    let before = timeline_to_string(&base).unwrap();
    let changed = |doc: &TimelineDoc| {
        let after = timeline_to_string(doc).unwrap();
        let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(a.len(), b.len(), "same number of lines:\n{after}");
        a.iter()
            .zip(&b)
            .filter(|(x, y)| x != y)
            .map(|(_, y)| y.to_string())
            .collect::<Vec<_>>()
    };
    let mut doc = base.clone();
    doc.tracks[0].keys[1].v = 45.0;
    assert_eq!(
        changed(&doc),
        [r#"    { t = 4.0, v = 45.0, interp = "bezier", out = [1.0, 0.0] },"#]
    );
    let mut doc = base.clone();
    doc.tracks[0].keys[0].interp = Interp::Step;
    assert_eq!(
        changed(&doc),
        [r#"    { t = 0.0, v = 0.0, interp = "step" },"#]
    );
    let mut doc = base.clone();
    doc.markers[0].name = "Break".into();
    assert_eq!(changed(&doc), [r#"name = "Break""#]);
    let mut doc = base.clone();
    doc.cues[0].t = Some(17.5);
    assert_eq!(changed(&doc), ["t = 17.5"]);
    let mut doc = base.clone();
    doc.cues[0].actions[1] = CueAction::Live {
        node: NodeId(4),
        param: "opacity".into(),
        value: json!(0.25),
    };
    assert_eq!(
        changed(&doc),
        [r#"    { node = 4, param = "opacity", live = 0.25 },"#]
    );

    // Adding a key adds one line.
    let mut doc = base.clone();
    doc.track_mut(NodeId(5), "color", Some(2)).set_key(2.0, 0.5);
    let after = timeline_to_string(&doc).unwrap();
    assert_eq!(after.lines().count(), before.lines().count() + 1);
    assert!(after.contains("    { t = 2.0, v = 0.5 },\n"));
}

#[test]
fn bad_files_are_rejected_with_a_location() {
    let cases = [
        ("format = 1\nlenght = 3.0\n", "unknown key `lenght`"),
        ("format = 2\n", "newer Atidarla"),
        ("format = 1\nlength = 0.0\n", "`length`"),
        ("format = 1\nloop = [4.0, 2.0]\n", "0 ≤ start < end"),
        (
            "format = 1\n[[track]]\nnode = 1\nparam = \"x\"\nkeys = [{ t = 1.0, v = 0.0 }, { t = 1.0, v = 2.0 }]\n",
            "two keys at 1 s",
        ),
        (
            "format = 1\n[[track]]\nnode = 1\nparam = \"x\"\nkeys = [{ t = -1.0, v = 0.0 }]\n",
            "track #1, key #1: `t` must be 0 or more",
        ),
        (
            "format = 1\n[[track]]\nnode = 1\nparam = \"x\"\nkeys = [{ t = 1.0, v = 0.0, interp = \"cubic\" }]\n",
            "`interp`",
        ),
        (
            "format = 1\n[[track]]\nnode = 1\nparam = \"x\"\n[[track]]\nnode = 1\nparam = \"x\"\n",
            "a second track",
        ),
        (
            "format = 1\n[[marker]]\nid = 1\nt = 0.0\n[[cue]]\nid = 1\n",
            "ID 1 is used twice",
        ),
        (
            "format = 1\n[[cue]]\nid = 1\nactions = [{ node = 1, param = \"x\" }]\n",
            "one of `set` or `live`",
        ),
        (
            "format = 1\n[[cue]]\nid = 1\nactions = [{ launch = 1, layers = [\"a\"] }]\n",
            "`layers`",
        ),
    ];
    for (text, want) in cases {
        let e = timeline_from_str(text).unwrap_err().to_string();
        assert!(e.contains(want), "{text}\n→ {e}\n(wanted {want})");
    }
    // Defaults: a minimal file is a 60 s, 60 fps empty timeline.
    assert_eq!(
        timeline_from_str("format = 1\n").unwrap(),
        TimelineDoc::default()
    );
}

#[test]
fn projects_save_and_load_their_timeline_and_load_unchanged_without_one() {
    let dir = std::env::temp_dir().join(format!("atd-format-timeline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut project = Project::new("Show");
    save(&dir, &project).unwrap();
    assert!(!dir.join(timeline::FILE).exists());
    assert_eq!(load(&dir).unwrap(), project);

    project.timeline = Some(timeline_from_str(SAMPLE).unwrap());
    save(&dir, &project).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join(timeline::FILE)).unwrap(),
        SAMPLE
    );
    assert_eq!(load(&dir).unwrap(), project);

    // A bad timeline names its file.
    std::fs::write(dir.join(timeline::FILE), "format = 1\nfps = 0.0\n").unwrap();
    let e = load(&dir).unwrap_err().to_string();
    assert!(e.starts_with("timeline.toml: "), "{e}");

    // Removing the timeline removes the file.
    project.timeline = None;
    save(&dir, &project).unwrap();
    assert!(!dir.join(timeline::FILE).exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn track_and_key_helpers_keep_order() {
    let mut doc = TimelineDoc::default();
    doc.track_mut(NodeId(4), "b", None);
    doc.track_mut(NodeId(2), "z", None);
    doc.track_mut(NodeId(4), "a", Some(1));
    doc.track_mut(NodeId(4), "a", None);
    let order: Vec<_> = doc
        .tracks
        .iter()
        .map(|t| (t.node.0, t.param.as_str(), t.component))
        .collect();
    assert_eq!(
        order,
        [
            (2, "z", None),
            (4, "a", None),
            (4, "a", Some(1)),
            (4, "b", None)
        ]
    );
    let t = doc.track_mut(NodeId(4), "a", None);
    t.set_key(2.0, 1.0);
    t.set_key(1.0, 0.0);
    assert_eq!(t.set_key(2.0002, 5.0), 1, "close enough to move the value");
    assert_eq!(t.keys, [Key::new(1.0, 0.0), Key::new(2.0, 5.0)]);
    let _ = Track::set_key;
}
