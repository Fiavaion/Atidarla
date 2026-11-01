//! Binding, track evaluation and cue firing (FR-T1, FR-T2).

use std::cell::Cell;

use lsr_core::graph::{Edit, Network, Node, NodeId, Param};
use lsr_core::registry::Registry;
use lsr_format::timeline::{Cue, CueAction, Interp, Key, TimelineDoc};
use lsr_timeline::{NoSignals, Signals, Timeline};
use serde_json::{Value, json};

fn registry() -> Registry {
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    reg
}

/// level1 (0), kaleido1 (1), constant1 (2, colour [0.1, 0.2, 0.3, 1]),
/// lfo1 (3), kaleido2 (4).
fn network(reg: &Registry) -> Network {
    let mut net = Network::default();
    for (ty, name) in [
        ("img.level", "level1"),
        ("img.kaleidoscope", "kaleido1"),
        ("img.constant", "constant1"),
        ("sig.lfo", "lfo1"),
        ("img.kaleidoscope", "kaleido2"),
    ] {
        let id = net.alloc_id();
        net.apply(
            reg,
            Edit::AddNode {
                node: Node::new(id, ty, name),
            },
        )
        .unwrap();
    }
    net.apply(
        reg,
        Edit::SetParam {
            node: NodeId(2),
            param: "colour".into(),
            value: Some(Param::constant(json!([0.1, 0.2, 0.3, 1.0]))),
        },
    )
    .unwrap();
    net
}

/// Reads stored parameters from a network, and one settable channel.
struct NetSignals<'a> {
    net: &'a Network,
    reg: &'a Registry,
    beat: Cell<f64>,
}

impl Signals for NetSignals<'_> {
    fn channel(&self, node: NodeId, channel: &str) -> Option<f64> {
        (node == NodeId(3) && channel == "lfo").then(|| self.beat.get())
    }
    fn param(&self, node: NodeId, param: &str, index: Option<usize>) -> Option<f64> {
        let v = self.net.param_value(self.reg, node, param)?;
        match index {
            Some(i) => v.get(i)?.as_f64(),
            None => v.as_f64(),
        }
    }
}

fn values(tl: &Timeline) -> Vec<(u64, String, Option<Value>)> {
    tl.slots()
        .iter()
        .enumerate()
        .map(|(i, s)| (s.node.0, s.param.clone(), tl.value(i).cloned()))
        .collect()
}

fn keys(list: &[(f64, f64)]) -> Vec<Key> {
    list.iter().map(|&(t, v)| Key::new(t, v)).collect()
}

#[test]
fn tracks_become_typed_live_values() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    doc.track_mut(NodeId(0), "brightness", None).keys = keys(&[(0.0, 0.0), (2.0, 1.0)]);
    doc.track_mut(NodeId(0), "invert", None).keys = keys(&[(0.0, 0.0), (1.0, 1.0)]);
    doc.track_mut(NodeId(1), "segments", None).keys = keys(&[(0.0, 2.0), (1.0, 5.0)]);
    let t = doc.track_mut(NodeId(2), "colour", Some(1));
    t.keys = keys(&[(0.0, 0.0), (1.0, 1.0)]);
    t.keys[0].interp = Interp::Step;
    let sig = NetSignals {
        net: &net,
        reg: &reg,
        beat: Cell::new(0.0),
    };
    let mut tl = Timeline::bind(doc, &net, &reg);
    assert!(tl.problems().is_empty(), "{:?}", tl.problems());
    tl.evaluate(0.5, &sig);
    assert_eq!(
        values(&tl),
        [
            (0, "brightness".into(), Some(json!(0.25))),
            (0, "invert".into(), Some(json!(true))),
            (1, "segments".into(), Some(json!(4))),
            // Unkeyed components keep the stored colour.
            (2, "colour".into(), Some(json!([0.1, 0.0, 0.3, 1.0]))),
        ]
    );
    tl.evaluate(0.25, &sig);
    assert_eq!(values(&tl)[1].2, Some(json!(false)));
    assert_eq!(values(&tl)[2].2, Some(json!(3)), "rounded");
    tl.evaluate(9.0, &sig);
    assert_eq!(values(&tl)[0].2, Some(json!(1.0)), "holds the last key");
    assert_eq!(values(&tl)[3].2, Some(json!([0.1, 1.0, 0.3, 1.0])));
}

#[test]
fn tracks_and_actions_that_cannot_bind_are_reported() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    doc.track_mut(NodeId(99), "x", None).set_key(0.0, 1.0);
    doc.track_mut(NodeId(0), "nope", None).set_key(0.0, 1.0);
    doc.track_mut(NodeId(3), "waveform", None).set_key(0.0, 1.0);
    doc.track_mut(NodeId(2), "colour", None).set_key(0.0, 1.0);
    doc.track_mut(NodeId(0), "gamma", Some(0)).set_key(0.0, 1.0);
    doc.cues.push(Cue {
        id: 1,
        name: "Bad".into(),
        t: Some(1.0),
        when: Some("[1, 2][0]".into()),
        actions: vec![CueAction::Launch {
            layers: vec![NodeId(0)],
            slot: 1,
        }],
    });
    let tl = Timeline::bind(doc, &net, &reg);
    let p = tl.problems().join("\n");
    for want in [
        "track node 99 `x`: no such node",
        "img.level has no parameter `nope`",
        "`waveform`: only numbers, toggles, vectors and colours animate",
        "`colour`: pick a component 0 to 3",
        "`gamma`: a number has no components",
        "cue 1 \"Bad\": node 0: img.level has no parameter `target`",
        "not in the native expression subset",
    ] {
        assert!(p.contains(want), "missing {want:?} in\n{p}");
    }
}

fn cue(id: u64, t: Option<f64>, when: Option<&str>, actions: Vec<CueAction>) -> Cue {
    Cue {
        id,
        name: format!("Q{id}"),
        t,
        when: when.map(str::to_owned),
        actions,
    }
}

fn live(node: u64, param: &str, v: Value) -> CueAction {
    CueAction::Live {
        node: NodeId(node),
        param: param.into(),
        value: v,
    }
}

/// Plays `tl` over `[0, end]` in steps of `dt`, returning (time, cue) for
/// every firing.
fn play(tl: &mut Timeline, from: f64, end: f64, dt: f64, sig: &dyn Signals) -> Vec<(f64, u64)> {
    let mut out = Vec::new();
    let mut t = from;
    let mut first = true;
    while t < end {
        let next = (t + dt).min(end);
        tl.begin();
        tl.cross(if first { from } else { t }, next, first);
        first = false;
        tl.finish(next, sig);
        for &i in tl.fired_now() {
            out.push((next, tl.doc().cues[i].id));
        }
        t = next;
    }
    out
}

#[test]
fn timed_cues_fire_exactly_once_whatever_the_frame_size() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    for (id, t) in [(1, 0.0), (2, 0.5), (3, 1.0), (4, 1.0 + 1e-9), (5, 2.0)] {
        doc.cues.push(cue(id, Some(t), None, vec![]));
    }
    for dt in [1.0 / 60.0, 1.0 / 24.0, 0.5, 0.3, 3.0] {
        let mut tl = Timeline::bind(doc.clone(), &net, &reg);
        let fired = play(&mut tl, 0.0, 2.5, dt, &NoSignals);
        let ids: Vec<u64> = fired.iter().map(|f| f.1).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5], "dt {dt}: {fired:?}");
        // Each fires on the first frame at or after its time.
        for (at, id) in fired {
            let t = doc.cues.iter().find(|c| c.id == id).unwrap().t.unwrap();
            assert!(at >= t && at - t < dt + 1e-9, "dt {dt}: cue {id} at {at}");
        }
        // Playing on fires nothing more.
        assert!(play(&mut tl, 2.5, 4.0, dt, &NoSignals).is_empty());
    }
}

#[test]
fn scrubbing_fires_nothing_unless_asked_and_rearming_replays() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    doc.cues.push(cue(
        1,
        Some(1.0),
        None,
        vec![live(0, "opacity", json!(0.5))],
    ));
    doc.cues.push(cue(
        2,
        Some(2.0),
        None,
        vec![live(0, "opacity", json!(0.2))],
    ));
    let mut tl = Timeline::bind(doc, &net, &reg);
    // A scrub is an evaluation at a time with no crossing.
    tl.begin();
    assert!(tl.finish(1.5, &NoSignals).is_empty());
    tl.evaluate(1.5, &NoSignals);
    assert_eq!(tl.value(0), None);
    // A chase fires what lies before, in order; the last held value wins.
    assert_eq!(tl.chase(2.5), [0, 1]);
    tl.evaluate(2.5, &NoSignals);
    assert_eq!(tl.value(0), Some(&json!(0.2)));
    // Jumping back and rearming lets cue 2 fire again; cue 1 does not.
    tl.rearm(1.5, f64::INFINITY, true);
    assert_eq!(play(&mut tl, 1.5, 3.0, 0.1, &NoSignals).len(), 1);
    // Reset (stop) clears held values.
    tl.reset();
    tl.evaluate(2.5, &NoSignals);
    assert_eq!(tl.value(0), None);
}

#[test]
fn conditions_arm_at_their_time_and_fire_when_true() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    doc.cues
        .push(cue(1, Some(1.0), Some("op('lfo1')['lfo'] > 0.5"), vec![]));
    // A manual cue with a condition fires whenever it holds (once).
    doc.cues.push(cue(
        2,
        None,
        Some("op('lfo1')['lfo'] < -0.5 and time > 3"),
        vec![],
    ));
    let sig = NetSignals {
        net: &net,
        reg: &reg,
        beat: Cell::new(1.0),
    };
    let mut tl = Timeline::bind(doc, &net, &reg);
    assert!(tl.problems().is_empty(), "{:?}", tl.problems());
    // True before its time: nothing (not armed yet).
    assert!(play(&mut tl, 0.0, 0.9, 0.1, &sig).is_empty());
    sig.beat.set(0.0);
    assert!(play(&mut tl, 0.9, 2.0, 0.1, &sig).is_empty());
    assert!(tl.armed(0));
    sig.beat.set(0.7);
    let fired = play(&mut tl, 2.0, 2.2, 0.1, &sig);
    assert_eq!(fired.len(), 1);
    assert!((fired[0].0 - 2.1).abs() < 1e-9);
    sig.beat.set(-1.0);
    assert!(
        play(&mut tl, 2.2, 2.9, 0.1, &sig).is_empty(),
        "time > 3 not yet"
    );
    assert_eq!(play(&mut tl, 2.9, 3.5, 0.1, &sig).len(), 1);
    assert!(play(&mut tl, 3.5, 5.0, 0.1, &sig).is_empty(), "once");
}

#[test]
fn go_fires_manual_cues_in_order() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    doc.cues
        .push(cue(5, None, None, vec![live(0, "gamma", json!(2.0))]));
    doc.cues
        .push(cue(3, None, None, vec![live(0, "gamma", json!(3.0))]));
    doc.cues.push(cue(9, Some(1.0), None, vec![]));
    doc.canonicalize();
    let mut tl = Timeline::bind(doc, &net, &reg);
    let ids = |tl: &mut Timeline| {
        tl.go();
        tl.begin();
        let f = tl.finish(0.0, &NoSignals).to_vec();
        f.iter().map(|&i| tl.doc().cues[i].id).collect::<Vec<_>>()
    };
    assert_eq!(ids(&mut tl), [3]);
    tl.evaluate(0.0, &NoSignals);
    assert_eq!(tl.value(0), Some(&json!(3.0)));
    assert_eq!(ids(&mut tl), [5]);
    assert_eq!(ids(&mut tl), Vec::<u64>::new(), "no manual cues left");
}

#[test]
fn tracks_win_over_values_held_by_cues() {
    let reg = registry();
    let net = network(&reg);
    let mut doc = TimelineDoc::default();
    doc.cues.push(cue(
        1,
        Some(0.0),
        None,
        vec![
            live(0, "brightness", json!(0.9)),
            live(2, "colour", json!([1.0, 1.0, 1.0, 1.0])),
        ],
    ));
    doc.track_mut(NodeId(2), "colour", Some(0))
        .set_key(0.0, 0.5);
    let mut tl = Timeline::bind(doc, &net, &reg);
    tl.chase(0.0);
    tl.evaluate(0.0, &NoSignals);
    let v = values(&tl);
    assert_eq!(v[0].2, Some(json!([0.5, 1.0, 1.0, 1.0])), "{v:?}");
    assert_eq!(v[1].2, Some(json!(0.9)));
}

#[test]
fn following_the_stage_fires_the_cue_matching_a_launch() {
    // kaleido1/kaleido2 stand in for layers (`img.layer` is built
    // elsewhere): give the operator a `target`.
    let mut reg = registry();
    let mut schema = reg.get("img.kaleidoscope").unwrap().clone();
    schema.params.push(lsr_core::registry::ParamSchema::int(
        "target",
        "Target",
        -1,
        Some(-1),
        Some(15),
    ));
    reg.replace(schema).unwrap();
    let mut net = network(&reg);
    let launch = |slot| CueAction::Launch {
        layers: vec![NodeId(1), NodeId(4)],
        slot,
    };
    let mut doc = TimelineDoc::default();
    doc.cues.push(cue(1, Some(4.0), None, vec![launch(0)]));
    doc.cues.push(cue(
        2,
        Some(8.0),
        None,
        vec![launch(1), live(0, "gamma", json!(2.0))],
    ));
    let mut tl = Timeline::bind(doc, &net, &reg);
    assert!(tl.problems().is_empty(), "{:?}", tl.problems());
    tl.set_follow_stage(true);
    let target = |net: &mut Network, v: i64| {
        for n in [1, 4] {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: NodeId(n),
                    param: "target".into(),
                    value: Some(Param::constant(json!(v))),
                },
            )
            .unwrap();
        }
    };
    let observe = |tl: &mut Timeline, net: &Network| {
        let sig = NetSignals {
            net,
            reg: &reg,
            beat: Cell::new(0.0),
        };
        tl.begin();
        let at = tl.observe_stage(&sig);
        tl.evaluate(0.0, &sig);
        at
    };
    assert_eq!(observe(&mut tl, &net), None, "first look only records");
    target(&mut net, 1);
    assert_eq!(
        observe(&mut tl, &net),
        Some(8.0),
        "the Stage launched cue 2"
    );
    assert_eq!(tl.current(), Some(1));
    // Its other actions ran; its launch was not repeated as a held value.
    let v = values(&tl);
    assert!(v.contains(&(0, "gamma".into(), Some(json!(2.0)))), "{v:?}");
    assert!(
        v.iter().all(|(_, p, x)| p != "target" || x.is_none()),
        "{v:?}"
    );
}
