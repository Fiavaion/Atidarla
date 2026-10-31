use super::*;
use serde_json::json;

const N: NodeId = NodeId(1);

fn cc(channel: u8, controller: u8, value: u8) -> Message {
    Message::Control {
        channel,
        controller,
        value,
    }
}

fn target(param: &str, component: Option<usize>) -> Target {
    Target {
        node: N,
        param: param.into(),
        component,
    }
}

fn midi_cc(n: u8) -> Source {
    Source::Midi {
        control: MidiControl::Cc(n),
        channel: None,
        device: String::new(),
    }
}

/// Parameters of one fake node, standing in for the engine: `frame`
/// runs the runtime and applies its writes as live values would.
struct Fake {
    params: HashMap<String, (ParamKind, Value)>,
    /// Components a binding or expression drives: (parameter, index).
    driven: HashMap<(String, usize), f64>,
}

impl Fake {
    fn new(params: &[(&str, ParamKind, Value)]) -> Self {
        Self {
            params: params
                .iter()
                .map(|(n, k, v)| ((*n).to_owned(), (k.clone(), v.clone())))
                .collect(),
            driven: HashMap::new(),
        }
    }

    fn frame(&mut self, rt: &mut MappingRuntime, maps: &Mappings) -> usize {
        rt.update(
            maps,
            |node, p| {
                (node == N)
                    .then(|| self.params.get(p).map(|(k, v)| (k, v)))
                    .flatten()
            },
            |node, p, i| {
                (node == N)
                    .then(|| self.driven.get(&(p.to_owned(), i)).copied())
                    .flatten()
            },
        );
        let writes: Vec<(String, Value)> = rt
            .writes()
            .map(|(_, p, v)| (p.to_owned(), v.clone()))
            .collect();
        let n = writes.len();
        for (p, v) in writes {
            self.params.get_mut(&p).unwrap().1 = v;
        }
        n
    }

    fn get(&self, p: &str) -> &Value {
        &self.params[p].1
    }
}

fn float() -> ParamKind {
    ParamKind::Float {
        min: Some(0.0),
        max: Some(1.0),
    }
}

#[test]
fn curves_shape_the_unit_range() {
    for c in [
        Curve::Linear,
        Curve::Exp(3.0),
        Curve::Exp(-3.0),
        Curve::Step(4),
    ] {
        assert_eq!(shape(c, 0.0), 0.0, "{c:?}");
        assert!((shape(c, 1.0) - 1.0).abs() < 1e-12, "{c:?}");
        let mut last = 0.0;
        for i in 0..=100 {
            let y = shape(c, f64::from(i) / 100.0);
            assert!(y >= last - 1e-12, "{c:?} is monotonic");
            last = y;
        }
    }
    assert!(shape(Curve::Exp(3.0), 0.5) < 0.25, "slow start");
    assert!(shape(Curve::Exp(-3.0), 0.5) > 0.75, "fast start");
    assert_eq!(shape(Curve::Exp(0.0), 0.3), 0.3);
    let steps: Vec<f64> = [0.0, 0.2, 0.3, 0.6, 0.8, 1.0]
        .iter()
        .map(|t| shape(Curve::Step(4), *t))
        .collect();
    assert_eq!(steps, [0.0, 0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0, 1.0]);
}

#[test]
fn ranges_clamp_and_may_be_reversed() {
    let mut maps = Mappings::default();
    maps.add(midi_cc(1), target("x", None), [10.0, -10.0]);
    let m = &maps.mappings[0];
    assert_eq!(map_value(m, 0.0), 10.0);
    assert_eq!(map_value(m, 127.0), -10.0);
    assert_eq!(map_value(m, 500.0), -10.0, "clamped");
    assert_eq!(normalise(0.25, [1.0, 0.0]), 0.75);
    assert_eq!(
        normalise(3.0, [2.0, 2.0]),
        1.0,
        "an empty range is a switch"
    );
}

#[test]
fn encoders_use_twos_complement() {
    let s = midi_cc(1);
    assert_eq!(relative_step(&s, 1.0), 1.0);
    assert_eq!(relative_step(&s, 5.0), 5.0);
    assert_eq!(relative_step(&s, 127.0), -1.0);
    assert_eq!(relative_step(&s, 65.0), -63.0);
    let osc = Source::Osc {
        address: "/enc".into(),
        arg: 0,
    };
    assert_eq!(relative_step(&osc, -0.25), -0.25);
}

#[test]
fn absolute_latest_wins_once_per_frame() {
    let mut maps = Mappings::default();
    maps.add(midi_cc(7), target("x", None), [0.0, 1.0]);
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("x", float(), json!(0.0))]);
    assert_eq!(fake.frame(&mut rt, &maps), 0, "nothing arrived");
    for v in [10, 64, 127] {
        rt.hub().push_midi("any", cc(1, 7, v));
    }
    assert_eq!(fake.frame(&mut rt, &maps), 1, "one write, not three");
    assert_eq!(fake.get("x"), &json!(1.0));
    assert_eq!(fake.frame(&mut rt, &maps), 0, "nothing new: no write");
    rt.hub().push_midi("any", cc(1, 8, 0));
    assert_eq!(fake.frame(&mut rt, &maps), 0, "other controller");
}

#[test]
fn channel_and_device_patterns_filter() {
    let mut maps = Mappings::default();
    maps.add(
        Source::Midi {
            control: MidiControl::Cc(1),
            channel: Some(2),
            device: "NANO".into(),
        },
        target("x", None),
        [0.0, 1.0],
    );
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("x", float(), json!(0.0))]);
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi("nanoKONTROL2", cc(1, 1, 127));
    rt.hub().push_midi("Launchpad", cc(2, 1, 127));
    assert_eq!(fake.frame(&mut rt, &maps), 0);
    rt.hub().push_midi("nanoKONTROL2", cc(2, 1, 127));
    assert_eq!(fake.frame(&mut rt, &maps), 1);
}

#[test]
fn toggle_flips_on_each_press() {
    let mut maps = Mappings::default();
    maps.add(
        Source::Midi {
            control: MidiControl::Note(36),
            channel: None,
            device: String::new(),
        },
        target("on", None),
        [0.0, 1.0],
    );
    maps.mappings[0].mode = Mode::Toggle;
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("on", ParamKind::Toggle, json!(false))]);
    let press = |rt: &MappingRuntime| {
        rt.hub().push_midi(
            "pads",
            Message::NoteOn {
                channel: 1,
                note: 36,
                velocity: 100,
            },
        );
        rt.hub().push_midi(
            "pads",
            Message::NoteOff {
                channel: 1,
                note: 36,
            },
        );
    };
    fake.frame(&mut rt, &maps);
    press(&rt);
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("on"), &json!(true));
    press(&rt);
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("on"), &json!(false));
    // Two presses within one frame: back where it was.
    press(&rt);
    press(&rt);
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("on"), &json!(false));
    // A CC toggles when it rises past the middle.
    maps.mappings[0].source = midi_cc(64);
    maps.mappings[0].input = [0.0, 127.0];
    rt.hub().push_midi("x", cc(1, 64, 127));
    rt.hub().push_midi("x", cc(1, 64, 100));
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi("x", cc(1, 64, 127));
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("on"), &json!(true), "held high: one press");
}

#[test]
fn momentary_follows_the_gate() {
    let mut maps = Mappings::default();
    maps.add(
        Source::Midi {
            control: MidiControl::Note(40),
            channel: None,
            device: String::new(),
        },
        target("x", None),
        [0.2, 0.9],
    );
    maps.mappings[0].mode = Mode::Momentary;
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("x", float(), json!(0.0))]);
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi(
        "k",
        Message::NoteOn {
            channel: 3,
            note: 40,
            velocity: 1,
        },
    );
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("x"), &json!(0.9));
    rt.hub().push_midi(
        "k",
        Message::NoteOff {
            channel: 3,
            note: 40,
        },
    );
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("x"), &json!(0.2));
}

#[test]
fn relative_encoders_add_steps_and_clamp() {
    let mut maps = Mappings::default();
    maps.add(midi_cc(20), target("x", None), [0.0, 1.27]);
    maps.mappings[0].mode = Mode::Relative;
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("x", float(), json!(0.5))]);
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi("enc", cc(1, 20, 3));
    rt.hub().push_midi("enc", cc(1, 20, 127));
    fake.frame(&mut rt, &maps);
    let x = fake.get("x").as_f64().unwrap();
    assert!((x - 0.52).abs() < 1e-9, "{x}");
    for _ in 0..10 {
        rt.hub().push_midi("enc", cc(1, 20, 63));
    }
    fake.frame(&mut rt, &maps);
    assert_eq!(fake.get("x"), &json!(1.27), "clamped to the output range");
}

#[test]
fn pickup_waits_until_the_control_reaches_the_value() {
    let mut maps = Mappings::default();
    maps.add(midi_cc(1), target("x", None), [0.0, 1.0]);
    maps.mappings[0].pickup = true;
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("x", float(), json!(0.5))]);
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi("f", cc(1, 1, 0));
    assert_eq!(fake.frame(&mut rt, &maps), 0, "far below: no jump");
    rt.hub().push_midi("f", cc(1, 1, 40));
    assert_eq!(fake.frame(&mut rt, &maps), 0);
    rt.hub().push_midi("f", cc(1, 1, 80));
    assert_eq!(fake.frame(&mut rt, &maps), 1, "passed the value: engaged");
    rt.hub().push_midi("f", cc(1, 1, 10));
    assert_eq!(fake.frame(&mut rt, &maps), 1, "engaged: follows");
    // Something else sets the parameter: wait again.
    fake.params.get_mut("x").unwrap().1 = json!(0.9);
    rt.hub().push_midi("f", cc(1, 1, 20));
    assert_eq!(fake.frame(&mut rt, &maps), 0);
    rt.hub().push_midi("f", cc(1, 1, 115));
    assert_eq!(fake.frame(&mut rt, &maps), 1, "within tolerance of 0.9");

    // Without pick-up the first value jumps.
    maps.mappings[0].pickup = false;
    rt.hub().push_midi("f", cc(1, 1, 0));
    assert_eq!(fake.frame(&mut rt, &maps), 1);
    assert_eq!(fake.get("x"), &json!(0.0));
}

#[test]
fn components_ints_and_menus_convert() {
    let mut maps = Mappings::default();
    maps.add(midi_cc(1), target("colour", Some(0)), [0.0, 1.0]);
    maps.add(midi_cc(2), target("colour", Some(2)), [0.0, 1.0]);
    maps.add(midi_cc(3), target("count", None), [0.0, 10.0]);
    maps.add(midi_cc(4), target("type", None), [0.0, 2.0]);
    let menu = ParamKind::Menu {
        options: vec!["a".into(), "b".into(), "c".into()],
    };
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[
        ("colour", ParamKind::Colour, json!([0.0, 0.0, 0.0, 1.0])),
        (
            "count",
            ParamKind::Int {
                min: None,
                max: None,
            },
            json!(0),
        ),
        ("type", menu, json!("a")),
    ]);
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi("d", cc(1, 1, 127));
    rt.hub().push_midi("d", cc(1, 2, 127));
    rt.hub().push_midi("d", cc(1, 3, 64));
    rt.hub().push_midi("d", cc(1, 4, 127));
    assert_eq!(fake.frame(&mut rt, &maps), 3, "two components, one write");
    assert_eq!(fake.get("colour"), &json!([1.0, 0.0, 1.0, 1.0]));
    assert_eq!(fake.get("count"), &json!(5));
    assert_eq!(fake.get("type"), &json!("c"));
    assert_eq!(
        live_value(&ParamKind::String, &json!(""), None, 1.0),
        None,
        "text is not mappable"
    );
}

#[test]
fn learn_maps_the_next_control() {
    let rt = MappingRuntime::new();
    let mut maps = Mappings::default();
    rt.arm(target("on", None));
    assert_eq!(rt.armed(), Some(target("on", None)));
    // A note-off is not a control to learn.
    rt.hub().push_midi(
        "pads",
        Message::NoteOff {
            channel: 1,
            note: 1,
        },
    );
    assert_eq!(rt.learn_into(&mut maps, |_| Some(ParamKind::Toggle)), None);
    rt.hub().push_midi(
        "pads",
        Message::NoteOn {
            channel: 10,
            note: 36,
            velocity: 90,
        },
    );
    let id = rt
        .learn_into(&mut maps, |_| Some(ParamKind::Toggle))
        .unwrap();
    assert_eq!(rt.armed(), None, "disarmed");
    let m = &maps.mappings[0];
    assert_eq!(m.id, id);
    assert_eq!(m.mode, Mode::Toggle);
    assert_eq!(
        m.source,
        Source::Midi {
            control: MidiControl::Note(36),
            channel: Some(10),
            device: "pads".into()
        }
    );
    // OSC learns the address; the next control does not learn again.
    rt.arm(target("level", None));
    rt.hub().push_osc("/fader/3", &[0.25]);
    rt.hub().push_osc("/fader/4", &[0.25]);
    rt.learn_into(&mut maps, |_| {
        Some(ParamKind::Float {
            min: Some(-1.0),
            max: Some(4.0),
        })
    })
    .unwrap();
    let m = &maps.mappings[1];
    assert_eq!(
        m.source,
        Source::Osc {
            address: "/fader/3".into(),
            arg: 0
        }
    );
    assert_eq!(m.output, [-1.0, 4.0]);
    assert_eq!(rt.take_learned(), None);
    rt.arm(target("x", None));
    rt.cancel_learn();
    rt.hub().push_osc("/fader/5", &[1.0]);
    assert_eq!(rt.take_learned(), None, "cancelled");
}

#[test]
fn the_monitor_keeps_the_last_messages() {
    let rt = MappingRuntime::new();
    for i in 0..(MONITOR_LEN + 10) {
        rt.hub().push_osc(&format!("/m/{i}"), &[i as f64]);
    }
    rt.hub().push_midi("dev", cc(5, 7, 1));
    let log = rt.monitor();
    assert_eq!(log.len(), MONITOR_LEN);
    assert_eq!(log.last().unwrap().text, "dev · CC 7 ch5 = 1");
    assert!(log[0].text.starts_with("OSC /m/11 "), "{}", log[0].text);
    assert_eq!(rt.hub().received(), MONITOR_LEN as u64 + 11);
}

#[test]
fn routes_set_parameters_directly() {
    let mut rt = MappingRuntime::new();
    rt.set_routes(vec![
        Route {
            address: "/fill/colour".into(),
            node: N,
            param: "colour".into(),
            kind: ParamKind::Colour,
        },
        Route {
            address: "/fill/count".into(),
            node: N,
            param: "count".into(),
            kind: ParamKind::Int {
                min: None,
                max: None,
            },
        },
    ]);
    let maps = Mappings::default();
    let mut fake = Fake::new(&[
        ("colour", ParamKind::Colour, json!([0.0, 0.0, 0.0, 1.0])),
        (
            "count",
            ParamKind::Int {
                min: None,
                max: None,
            },
            json!(0),
        ),
    ]);
    rt.hub().push_osc("/fill/colour", &[0.5, 0.25, 1.0]);
    rt.hub().push_osc("/fill/count", &[2.6]);
    rt.hub().push_osc("/unknown", &[1.0]);
    assert_eq!(fake.frame(&mut rt, &maps), 2);
    assert_eq!(fake.get("colour"), &json!([0.5, 0.25, 1.0, 1.0]));
    assert_eq!(fake.get("count"), &json!(3));
}

#[test]
fn osc_over_loopback_reaches_a_mapping() {
    let mut maps = Mappings::default();
    maps.add(
        Source::Osc {
            address: "/level".into(),
            arg: 1,
        },
        target("x", None),
        [0.0, 2.0],
    );
    let mut rt = MappingRuntime::new();
    let port = rt.listen_osc(0, true).unwrap();
    assert_eq!(rt.osc_port(), Some(port));
    let mut fake = Fake::new(&[("x", float(), json!(0.0))]);
    fake.frame(&mut rt, &maps);
    let msg = rosc::encoder::encode(&OscPacket::Message(rosc::OscMessage {
        addr: "/level".into(),
        args: vec![
            OscType::String("skip".into()),
            OscType::Int(9),
            OscType::Float(0.5),
        ],
    }))
    .unwrap();
    let tx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    tx.send_to(&msg, (Ipv4Addr::LOCALHOST, port)).unwrap();
    let start = Instant::now();
    while fake.frame(&mut rt, &maps) == 0 {
        assert!(start.elapsed() < Duration::from_secs(5), "no OSC arrived");
        std::thread::sleep(Duration::from_millis(1));
    }
    // Numeric arguments only: [9, 0.5]; arg 1 is 0.5 → 1.0.
    assert_eq!(fake.get("x"), &json!(1.0));
}

/// What a mapping wrote is kept for saving across edits of the mapping,
/// and released once no mapping drives the parameter.
#[test]
fn written_and_released_follow_the_mappings() {
    let mut maps = Mappings::default();
    let id = maps.add(midi_cc(7), target("x", None), [0.0, 1.0]);
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("x", float(), json!(0.0))]);
    fake.frame(&mut rt, &maps);
    assert_eq!(rt.written().count(), 0, "nothing written yet");
    rt.hub().push_midi("d", cc(1, 7, 127));
    fake.frame(&mut rt, &maps);
    let written: Vec<(String, f64)> = rt.written().map(|(t, y)| (t.param.clone(), y)).collect();
    assert_eq!(written, [("x".to_owned(), 1.0)]);
    maps.mappings[0].output = [0.0, 2.0];
    fake.frame(&mut rt, &maps);
    assert_eq!(rt.written().count(), 1, "kept across a range edit");
    assert_eq!(rt.released().count(), 0, "still driven");
    maps.remove(id);
    fake.frame(&mut rt, &maps);
    assert_eq!(rt.written().count(), 0);
    let released: Vec<(NodeId, String, Value)> = rt
        .released()
        .map(|(n, p, v)| (n, p.to_owned(), v.clone()))
        .collect();
    assert_eq!(released, [(N, "x".to_owned(), json!(1.0))]);
    fake.frame(&mut rt, &maps);
    assert_eq!(rt.released().count(), 0, "released once");
}

#[test]
fn a_mapped_component_leaves_the_others_following_their_binding() {
    let mut maps = Mappings::default();
    maps.add(midi_cc(1), target("scale", Some(0)), [0.0, 1.0]);
    maps.mappings[0].pickup = false;
    let mut rt = MappingRuntime::new();
    let mut fake = Fake::new(&[("scale", ParamKind::Vec { n: 2 }, json!([1.0, 1.0]))]);
    // An LFO bound to the whole vector drives both components.
    let lfo = |t: f64| (t * 0.7).sin();
    let drive = |fake: &mut Fake, t: f64| {
        for c in 0..2 {
            fake.driven.insert(("scale".into(), c), lfo(t));
        }
    };
    drive(&mut fake, 0.0);
    fake.frame(&mut rt, &maps);
    rt.hub().push_midi("k", cc(1, 1, 127));
    assert_eq!(fake.frame(&mut rt, &maps), 1);
    assert_eq!(fake.get("scale"), &json!([1.0, lfo(0.0)]));
    for f in 1..5 {
        let t = f64::from(f);
        drive(&mut fake, t);
        assert_eq!(fake.frame(&mut rt, &maps), 1, "the LFO moved");
        assert_eq!(
            fake.get("scale"),
            &json!([1.0, lfo(t)]),
            "the knob holds [0], [1] follows the LFO"
        );
    }
    assert_eq!(fake.frame(&mut rt, &maps), 0, "nothing moved: no write");
    // The knob moves again: [1] is still the LFO's.
    drive(&mut fake, 9.0);
    rt.hub().push_midi("k", cc(1, 1, 0));
    assert_eq!(fake.frame(&mut rt, &maps), 1, "one write per parameter");
    assert_eq!(fake.get("scale"), &json!([0.0, lfo(9.0)]));
}

#[test]
fn a_route_fills_components_it_does_not_set_from_their_binding() {
    let mut rt = MappingRuntime::new();
    rt.set_routes(vec![Route {
        address: "/s".into(),
        node: N,
        param: "scale".into(),
        kind: ParamKind::Vec { n: 3 },
    }]);
    let maps = Mappings::default();
    let mut fake = Fake::new(&[("scale", ParamKind::Vec { n: 3 }, json!([1.0, 1.0, 1.0]))]);
    fake.driven.insert(("scale".into(), 0), 0.25);
    fake.driven.insert(("scale".into(), 2), 0.75);
    rt.hub().push_osc("/s", &[0.5]);
    assert_eq!(fake.frame(&mut rt, &maps), 1);
    assert_eq!(
        fake.get("scale"),
        &json!([0.5, 1.0, 0.75]),
        "[0] from OSC, [1] stored, [2] driven"
    );
}
