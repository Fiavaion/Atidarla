use super::*;
use crate::cook::Scheduler;
use crate::graph::tests::{connect, test_registry};
use crate::graph::{ChannelRef, Comment};
use crate::history::History;
use crate::registry::ParamSchema;
use serde_json::json;

/// The graph test operators plus one with int and menu parameters.
fn base() -> Registry {
    let mut r = test_registry();
    let mut pick = r.get("img.mix").unwrap().clone();
    pick.type_id = "img.pick".into();
    pick.params
        .push(ParamSchema::int("count", "Count", 2, Some(0), Some(8)));
    pick.params
        .push(ParamSchema::menu("mode", "Mode", &["a", "b"], "a"));
    r.register(pick).unwrap();
    r
}

fn node(net: &mut Network, reg: &Registry, ty: &str, name: &str) -> NodeId {
    let id = net.alloc_id();
    net.apply(
        reg,
        Edit::AddNode {
            node: Node::new(id, ty, name),
        },
    )
    .unwrap();
    id
}

fn set(net: &mut Network, reg: &Registry, n: NodeId, param: &str, p: Param) {
    net.apply(
        reg,
        Edit::SetParam {
            node: n,
            param: param.into(),
            value: Some(p),
        },
    )
    .unwrap();
}

/// `glow`: in → mix.a, gen → mix.b; out = mix.out; promoted `amount`
/// drives mix.gain (0…5), `seed` drives gen.gain.
fn glow() -> PackDef {
    let reg = base();
    let mut net = Network::default();
    let gen_ = node(&mut net, &reg, "img.gen", "gen1");
    let mix = node(&mut net, &reg, "img.mix", "mix1");
    connect(&mut net, &reg, gen_, mix, "b");
    set(&mut net, &reg, mix, "gain", Param::constant(json!(2.0)));
    PackDef {
        version: Version::new(1, 0, 0),
        label: "Glow".into(),
        summary: "Test glow.".into(),
        help: "# Glow\nMixes.\n".into(),
        locked: true,
        params: vec![
            PromotedParam {
                name: "amount".into(),
                label: "Amount".into(),
                default: json!(1.5),
                min: Some(0.0),
                max: Some(5.0),
                help: "How much.".into(),
                targets: vec![ParamTarget::new(mix, "gain")],
            },
            PromotedParam {
                name: "seed".into(),
                label: String::new(),
                default: json!(1.0),
                min: None,
                max: None,
                help: String::new(),
                targets: vec![ParamTarget::new(gen_, "gain")],
            },
        ],
        inputs: vec![PackInput {
            name: "in".into(),
            targets: vec![Endpoint::new(mix, "a")],
        }],
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(mix, "out"),
        }],
        network: net,
        ..PackDef::new("glow")
    }
}

fn set_of(defs: impl IntoIterator<Item = PackDef>) -> PackSet {
    let mut s = PackSet::default();
    for d in defs {
        s.insert(d);
    }
    s
}

#[test]
fn versions_parse_print_and_match() {
    let v: Version = "1.2.3".parse().unwrap();
    assert_eq!(v, Version::new(1, 2, 3));
    assert_eq!(v.to_string(), "1.2.3");
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.2.x",
        "-1.2.3",
        "1.2.3-beta",
        " 1.2.3",
        "9999999999.0.0",
    ] {
        assert!(bad.parse::<Version>().is_err(), "{bad}");
    }
    let req = |s: &str| s.parse::<VersionReq>().unwrap();
    assert!(req("^1.2").matches(Version::new(1, 9, 0)));
    assert!(!req("^1.2").matches(Version::new(2, 0, 0)));
    assert!(!req("^1.2").matches(Version::new(1, 1, 9)));
    assert!(req("^0.3").matches(Version::new(0, 3, 7)));
    assert!(!req("^0.3").matches(Version::new(0, 4, 0)));
    assert!(req("1.2.3").matches(v) && !req("1.2.3").matches(Version::new(1, 2, 4)));
    assert!(req("*").matches(Version::new(0, 0, 1)));
    assert_eq!(req("^1").to_string(), "^1.0.0");
    assert!("^1.x".parse::<VersionReq>().is_err());
    assert!(Version::new(1, 0, 0).compatible_with(Version::new(1, 4, 2)));
    assert!(!Version::new(1, 0, 0).compatible_with(Version::new(2, 0, 0)));
    let r: PackRef = "glow@1.0.0".parse().unwrap();
    assert_eq!(r.to_string(), "glow@1.0.0");
    assert!("Glow@1.0.0".parse::<PackRef>().is_err());
}

#[test]
fn a_pack_becomes_an_operator_schema() {
    let packs = set_of([glow()]);
    let reg = packs.register(&base()).unwrap();
    let s = reg.get("cmp.pack_glow").unwrap();
    assert_eq!(s.family, OpFamily::Cmp);
    assert_eq!(s.label, "Glow");
    assert_eq!(s.version, 1);
    let amount = &s.params[0];
    assert_eq!(
        amount.kind,
        ParamKind::Float {
            min: Some(0.0),
            max: Some(5.0)
        }
    );
    assert_eq!(
        (amount.label.as_str(), amount.help.as_str()),
        ("Amount", "How much.")
    );
    // Unset label and help come from the target; the range is the target's.
    let seed = &s.params[1];
    assert_eq!(
        (seed.label.as_str(), &seed.kind),
        (
            "Gain",
            &ParamKind::Float {
                min: Some(0.0),
                max: Some(10.0)
            }
        )
    );
    assert_eq!(s.inputs[0].wire, WireType::Img);
    assert!(!s.inputs[0].feedback, "in reaches the output");
    assert_eq!(s.outputs[0].wire, WireType::Img);
    assert_eq!(s.cook_budget_us, 200);
}

#[test]
fn invalid_definitions_are_errors_that_name_the_pack() {
    let reg = base();
    let check = |def: PackDef, needle: &str| {
        let err = set_of([def]).register(&reg).unwrap_err().to_string();
        assert!(err.contains(needle), "expected `{needle}` in: {err}");
        assert!(err.contains("glow") || err.contains("Glow"), "{err}");
    };
    let mut d = glow();
    d.params[0].targets[0].param = "nope".into();
    check(d, "has no parameter `nope`");
    let mut d = glow();
    d.params[0].targets[0].node = NodeId(99);
    check(d, "node n99, which is not in the Pack");
    let mut d = glow();
    d.params[0].max = Some(50.0);
    check(d, "max 50 is outside");
    let mut d = glow();
    d.params[0].default = json!(7.0);
    check(d, "default");
    let mut d = glow();
    d.params[1].name = "amount".into();
    check(d, "declared twice");
    let mut d = glow();
    d.params[1].targets = d.params[0].targets.clone();
    check(d, "driven by two");
    let mut d = glow();
    d.params[0].name = "Bad Name".into();
    check(d, "parameter `Bad Name`");
    let mut d = glow();
    d.inputs[0].targets[0].socket = "b".into();
    check(d, "already wired inside");
    let mut d = glow();
    d.inputs[0].targets.clear();
    check(d, "feeds no inner input");
    let mut d = glow();
    d.outputs[0].source.socket = "zz".into();
    check(d, "has no output `zz`");
    let mut d = glow();
    d.name = "Glow!".into();
    assert!(matches!(
        set_of([d]).register(&reg),
        Err(PackError::InvalidName(_))
    ));
    // Two targets of different kinds.
    let mut d = glow();
    let pick = node(&mut d.network, &reg, "img.pick", "pick1");
    d.params[0].targets.push(ParamTarget::new(pick, "count"));
    check(d, "differ in kind");
    // Min/max on a menu.
    let mut d = glow();
    let pick = node(&mut d.network, &reg, "img.pick", "pick1");
    d.params.push(PromotedParam {
        name: "mode".into(),
        label: String::new(),
        default: json!("b"),
        min: Some(0.0),
        max: None,
        help: String::new(),
        targets: vec![ParamTarget::new(pick, "mode")],
    });
    check(d, "only number parameters");
    // Inner network errors.
    let mut d = glow();
    d.network = serde_json::from_value(json!({
        "next_id": 1, "nodes": [{"id": 0, "type_id": "img.nope", "name": "x", "pos": [0, 0]}], "wires": []
    }))
    .unwrap();
    check(d, "unknown operator type `img.nope`");
}

#[test]
fn int_ranges_narrow_and_menus_promote() {
    let reg = base();
    let mut d = glow();
    let pick = node(&mut d.network, &reg, "img.pick", "pick1");
    d.params.push(PromotedParam {
        name: "count".into(),
        label: String::new(),
        default: json!(3),
        min: Some(1.0),
        max: None,
        help: String::new(),
        targets: vec![ParamTarget::new(pick, "count")],
    });
    d.params.push(PromotedParam {
        name: "mode".into(),
        label: String::new(),
        default: json!("b"),
        min: None,
        max: None,
        help: String::new(),
        targets: vec![ParamTarget::new(pick, "mode")],
    });
    let packs = set_of([d.clone()]);
    let r = packs.register(&reg).unwrap();
    let s = r.get("cmp.pack_glow").unwrap();
    assert_eq!(
        s.params[2].kind,
        ParamKind::Int {
            min: Some(1),
            max: Some(8)
        }
    );
    assert_eq!(s.params[3].default, json!("b"));
    d.params[2].min = Some(1.5);
    assert!(
        set_of([d])
            .register(&reg)
            .unwrap_err()
            .to_string()
            .contains("whole-number")
    );
}

/// A chain of Packs, each containing the next.
fn chain(n: usize) -> PackSet {
    let reg = base();
    let mut set = PackSet::default();
    for i in 0..n {
        let mut net = Network::default();
        let ty = if i + 1 < n {
            type_id(&format!("p{}", i + 1))
        } else {
            "img.gen".into()
        };
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(NodeId(0), ty, "inner1"),
            },
        )
        .ok();
        // Untyped add (the registry does not know the inner Pack yet).
        if net.is_empty() {
            net = serde_json::from_value(json!({
                "next_id": 1,
                "nodes": [{"id": 0, "type_id": type_id(&format!("p{}", i + 1)), "name": "inner1", "pos": [0, 0]}],
                "wires": []
            }))
            .unwrap();
        }
        set.insert(PackDef {
            outputs: vec![PackOutput {
                name: "out".into(),
                source: Endpoint::new(NodeId(0), "out"),
            }],
            network: net,
            ..PackDef::new(format!("p{i}"))
        });
    }
    set
}

#[test]
fn nesting_cycles_depth_and_unknown_packs_are_errors() {
    let reg = base();
    // MAX_DEPTH nested Packs are fine and flatten to one node.
    let ok = chain(MAX_DEPTH);
    let r = ok.register(&reg).unwrap();
    let mut top = Network::default();
    let inst = node(&mut top, &r, "cmp.pack_p0", "p0_1");
    let flat = ok.flatten(&top, &r).unwrap();
    assert_eq!(flat.network.len(), 1);
    let only = flat.network.nodes().next().unwrap();
    assert_eq!(only.type_id, "img.gen");
    assert!(
        only.name.starts_with("p0_1__inner1__inner1__"),
        "{}",
        only.name
    );
    assert_eq!(flat.view(inst), Some(only.id));
    assert_eq!(flat.instance_of(only.id), Some(inst));
    assert!(flat.display(only.id).starts_with("p0_1/inner1/"));

    // One more is too deep; a very deep chain is an error, not a stack overflow.
    for n in [MAX_DEPTH + 1, 5000] {
        let err = chain(n).register(&reg).unwrap_err();
        assert!(matches!(err, PackError::TooDeep(_)), "{n}: {err}");
    }

    // Cycles, including a Pack containing itself.
    let mut c = chain(3);
    let mut last = c.get("p2").unwrap().clone();
    last.network = serde_json::from_value(json!({
        "next_id": 1, "nodes": [{"id": 0, "type_id": "cmp.pack_p0", "name": "back", "pos": [0, 0]}], "wires": []
    }))
    .unwrap();
    c.insert(last);
    let err = c.register(&reg).unwrap_err();
    assert_eq!(err, PackError::Cycle("p0 → p1 → p2 → p0".into()));
    let mut me = chain(1);
    let mut d = me.get("p0").unwrap().clone();
    d.network = serde_json::from_value(json!({
        "next_id": 1, "nodes": [{"id": 0, "type_id": "cmp.pack_p0", "name": "me", "pos": [0, 0]}], "wires": []
    }))
    .unwrap();
    me.insert(d);
    assert!(matches!(me.register(&reg), Err(PackError::Cycle(_))));

    // Unknown inner Pack.
    let mut u = chain(2);
    u.remove("p1");
    let err = u.register(&reg).unwrap_err().to_string();
    assert!(err.contains("unknown Pack `p1`"), "{err}");
}

/// main: lfo, src(gen) → glow1.in; glow1.out → mix.a; glow1.amount bound to lfo.
fn project() -> (PackSet, Registry, Network, [NodeId; 4]) {
    let packs = set_of([glow()]);
    let reg = packs.register(&base()).unwrap();
    let mut net = Network::default();
    let lfo = node(&mut net, &reg, "sig.lfo", "lfo1");
    let src = node(&mut net, &reg, "img.gen", "src1");
    let g = node(&mut net, &reg, "cmp.pack_glow", "glow1");
    let mix = node(&mut net, &reg, "img.mix", "mix1");
    net.apply(
        &reg,
        Edit::Connect {
            wire: Wire::new(Endpoint::new(src, "out"), Endpoint::new(g, "in")),
        },
    )
    .unwrap();
    net.apply(
        &reg,
        Edit::Connect {
            wire: Wire::new(Endpoint::new(g, "out"), Endpoint::new(mix, "a")),
        },
    )
    .unwrap();
    set(
        &mut net,
        &reg,
        g,
        "amount",
        Param::bound(json!(2.0), lfo, "lfo"),
    );
    (packs, reg, net, [lfo, src, g, mix])
}

#[test]
fn instances_wire_validate_and_round_trip_like_operators() {
    let (_, reg, mut net, [lfo, src, g, mix]) = project();
    // Wrong wire type into the Pack.
    let err = net
        .apply(
            &reg,
            Edit::Connect {
                wire: Wire::new(Endpoint::new(lfo, "out"), Endpoint::new(g, "in")),
            },
        )
        .unwrap_err();
    assert!(matches!(err, GraphError::WireTypeMismatch { .. }));
    // A loop through the Pack is a cycle.
    let err = net
        .apply(
            &reg,
            Edit::Connect {
                wire: Wire::new(Endpoint::new(mix, "out"), Endpoint::new(g, "in")),
            },
        )
        .unwrap_err();
    assert!(matches!(err, GraphError::Cycle(_)), "{err}");
    // Promoted parameters validate against the promoted range.
    let err = net
        .apply(
            &reg,
            Edit::SetParam {
                node: g,
                param: "amount".into(),
                value: Some(Param::constant(json!(6.0))),
            },
        )
        .unwrap_err();
    assert!(matches!(err, GraphError::InvalidValue(_)));
    // Undo/redo through the history, like any node.
    let mut h = History::default();
    let before = net.clone();
    h.commit(
        &mut net,
        &reg,
        "amount",
        [Edit::SetParam {
            node: g,
            param: "seed".into(),
            value: Some(Param::constant(json!(3.0))),
        }],
    )
    .unwrap();
    h.commit(&mut net, &reg, "remove", [Edit::RemoveNode { id: g }])
        .unwrap();
    assert!(net.wires().all(|w| w.to.node != g && w.from.node != g));
    h.undo(&mut net, &reg).unwrap();
    h.undo(&mut net, &reg).unwrap();
    assert_eq!(net, before);
    let _ = src;
    // JSON round trip through validation.
    let text = serde_json::to_string(&net).unwrap();
    let back: Network = serde_json::from_str(&text).unwrap();
    assert_eq!(back.validated(&reg).unwrap(), net);
}

#[test]
fn flattening_expands_instances_with_stable_ids() {
    let (packs, reg, net, [lfo, src, g, mix]) = project();
    let flat = packs.flatten(&net, &reg).unwrap();
    let f = &flat.network;
    assert_eq!(f.len(), 5, "lfo, src, mix and the Pack's gen and mix");
    for id in [lfo, src, mix] {
        assert_eq!(
            f.node(id).unwrap().name,
            net.node(id).unwrap().name,
            "outer nodes keep IDs and names"
        );
    }
    assert!(f.node(g).is_none(), "the instance itself is not cooked");
    let inner_mix = f.node_by_name("glow1__mix1").unwrap();
    let inner_gen = f.node_by_name("glow1__gen1").unwrap();
    assert!(inner_mix.id.0 >= 1 << 62 && inner_gen.id.0 >= 1 << 62);
    assert_eq!(flat.display(inner_mix.id), "glow1/mix1");
    assert_eq!(
        flat.output(g, "out"),
        Some(&Endpoint::new(inner_mix.id, "out"))
    );
    assert_eq!(flat.view(g), Some(inner_mix.id));
    assert_eq!(flat.view(mix), Some(mix));
    // The promoted value (with its binding) replaces the inner value; the
    // unset promoted parameter takes its default.
    assert_eq!(
        inner_mix.params["gain"],
        Param::bound(json!(2.0), lfo, "lfo")
    );
    assert!(
        !inner_gen.params.contains_key("gain"),
        "seed default 1.0 is the operator default"
    );
    // Wires through the Pack boundary.
    let wires: BTreeSet<(String, String, String)> = f
        .wires()
        .map(|w| {
            (
                f.node(w.from.node).unwrap().name.clone(),
                f.node(w.to.node).unwrap().name.clone(),
                w.to.socket,
            )
        })
        .collect();
    assert_eq!(
        wires,
        BTreeSet::from([
            ("src1".into(), "glow1__mix1".into(), "a".into()),
            ("glow1__gen1".into(), "glow1__mix1".into(), "b".into()),
            ("glow1__mix1".into(), "mix1".into(), "a".into()),
        ])
    );
    // Deterministic, and inner IDs survive unrelated edits.
    assert_eq!(packs.flatten(&net, &reg).unwrap(), flat);
    let mut edited = net.clone();
    node(&mut edited, &reg, "img.gen", "extra1");
    let again = packs.flatten(&edited, &reg).unwrap();
    assert_eq!(
        again.network.node_by_name("glow1__mix1").unwrap().id,
        inner_mix.id
    );

    // The scheduler plans the flat network: inputs first, deterministic.
    let mut s = Scheduler::default();
    let mut plan = Vec::new();
    s.plan(f, &reg, &[mix], &mut plan).unwrap();
    let names: Vec<&str> = plan
        .iter()
        .map(|id| f.node(*id).unwrap().name.as_str())
        .collect();
    assert_eq!(
        names,
        ["src1", "glow1__gen1", "lfo1", "glow1__mix1", "mix1"]
    );
    for &n in &plan {
        s.record(n, std::time::Duration::from_micros(1));
    }
    s.plan(f, &reg, &[mix], &mut plan).unwrap();
    assert_eq!(
        plan.len(),
        3,
        "the time-dependent LFO re-cooks the bound mix and what follows: {plan:?}"
    );
}

#[test]
fn nested_packs_promote_through_and_rewrite_expressions() {
    let reg0 = base();
    // `outer` contains a glow instance and an lfo; its `level` promotes
    // glow's `amount`; an inner expression names the lfo.
    let mut packs = set_of([glow()]);
    let reg1 = packs.register(&reg0).unwrap();
    let mut net = Network::default();
    let lfo = node(&mut net, &reg1, "sig.lfo", "lfo1");
    let gi = node(&mut net, &reg1, "cmp.pack_glow", "g1");
    let tail = node(&mut net, &reg1, "img.mix", "tail1");
    connect(&mut net, &reg1, gi, tail, "a");
    set(
        &mut net,
        &reg1,
        tail,
        "gain",
        Param {
            value: json!(1.0),
            expr: Some("op('lfo1')['lfo'] + op('g1')['x'] + op('nope')['y']".into()),
            bind: None,
        },
    );
    let _ = lfo;
    packs.insert(PackDef {
        params: vec![PromotedParam {
            name: "level".into(),
            label: String::new(),
            default: json!(0.5),
            min: None,
            max: None,
            help: String::new(),
            targets: vec![ParamTarget::new(gi, "amount")],
        }],
        inputs: vec![PackInput {
            name: "src".into(),
            targets: vec![Endpoint::new(gi, "in")],
        }],
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(tail, "out"),
        }],
        network: net,
        ..PackDef::new("outer")
    });
    let reg = packs.register(&reg0).unwrap();
    let s = reg.get("cmp.pack_outer").unwrap();
    assert_eq!(
        s.params[0].kind,
        ParamKind::Float {
            min: Some(0.0),
            max: Some(5.0)
        },
        "range from the inner promoted parameter"
    );

    let mut top = Network::default();
    let a = node(&mut top, &reg, "cmp.pack_outer", "a1");
    let b = node(&mut top, &reg, "cmp.pack_outer", "b1");
    connect(&mut top, &reg, a, b, "src");
    set(
        &mut top,
        &reg,
        b,
        "level",
        Param {
            value: json!(1.0),
            expr: Some("me.digits * 2".into()),
            bind: None,
        },
    );
    let flat = packs.flatten(&top, &reg).unwrap();
    let f = &flat.network;
    assert_eq!(
        f.len(),
        8,
        "{:?}",
        f.nodes().map(|n| &n.name).collect::<Vec<_>>()
    );
    // a1 leaves level at 0.5 → glow amount 0.5 → inner mix gain 0.5.
    assert_eq!(
        f.node_by_name("a1__g1__mix1").unwrap().params["gain"].value,
        json!(0.5)
    );
    // b1's expression goes all the way down.
    let deep = f.node_by_name("b1__g1__mix1").unwrap();
    assert_eq!(deep.params["gain"].expr.as_deref(), Some("me.digits * 2"));
    // Inner references are renamed for the scope; unknown names are kept.
    let tail_b = f.node_by_name("b1__tail1").unwrap();
    assert_eq!(
        tail_b.params["gain"].expr.as_deref(),
        Some("op('b1__lfo1')['lfo'] + op('g1')['x'] + op('nope')['y']"),
        "g1 has no SIG output, so its name stays (and errors when cooked)"
    );
    // a1.out → b1.src reaches b1's inner glow mix input `a`.
    let a_tail = f.node_by_name("a1__tail1").unwrap().id;
    let b_mix = deep.id;
    assert!(
        f.wires()
            .any(|w| w.from.node == a_tail && w.to == Endpoint::new(b_mix, "a"))
    );
    assert_eq!(flat.display(b_mix), "b1/g1/mix1");
}

#[test]
fn feedback_through_a_pack_is_allowed_only_via_feedback_inputs() {
    let reg0 = base();
    // `trail`: in → fb.in; mix(fb.out, loop) ; out = fb.out; `loop` input
    // feeds fb.prev (a feedback socket), so it may close a loop outside.
    let mut net = Network::default();
    let fb = node(&mut net, &reg0, "img.feedback", "fb1");
    let packs = set_of([PackDef {
        inputs: vec![
            PackInput {
                name: "in".into(),
                targets: vec![Endpoint::new(fb, "in")],
            },
            PackInput {
                name: "loop".into(),
                targets: vec![Endpoint::new(fb, "prev")],
            },
        ],
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(fb, "out"),
        }],
        network: net,
        ..PackDef::new("trail")
    }]);
    let reg = packs.register(&reg0).unwrap();
    let s = reg.get("cmp.pack_trail").unwrap();
    assert!(!s.inputs[0].feedback && s.inputs[1].feedback);
    let mut top = Network::default();
    let g = node(&mut top, &reg, "img.gen", "src1");
    let t = node(&mut top, &reg, "cmp.pack_trail", "trail1");
    let m = node(&mut top, &reg, "img.mix", "mix1");
    connect(&mut top, &reg, g, t, "in");
    top.apply(
        &reg,
        Edit::Connect {
            wire: Wire::new(Endpoint::new(t, "out"), Endpoint::new(m, "a")),
        },
    )
    .unwrap();
    assert!(
        top.apply(
            &reg,
            Edit::Connect {
                wire: Wire::new(Endpoint::new(m, "out"), Endpoint::new(t, "in"))
            }
        )
        .is_err()
    );
    top.apply(
        &reg,
        Edit::Connect {
            wire: Wire::new(Endpoint::new(m, "out"), Endpoint::new(t, "loop")),
        },
    )
    .unwrap();
    let flat = packs.flatten(&top, &reg).unwrap();
    let mut s = Scheduler::default();
    let mut plan = Vec::new();
    s.plan(&flat.network, &reg, &[m], &mut plan).unwrap();
    assert_eq!(plan.len(), 3);
}

#[test]
fn locked_packs_refuse_inner_edits_and_unlock_gives_a_copy() {
    let (mut packs, reg, net, [_, _, g, _]) = project();
    let base = base();
    let inner_mix = NodeId(1);
    let err = packs
        .edit(
            "glow",
            &base,
            [Edit::Move {
                node: inner_mix,
                pos: [1.0, 1.0],
            }],
        )
        .unwrap_err();
    assert_eq!(err, PackError::Locked("glow".into()));
    assert!(err.to_string().contains("locked"));

    let (copy, edits) = packs.unlock(&net, g).unwrap();
    assert_eq!(copy, "glow_local1");
    let d = packs.get(&copy).unwrap();
    assert!(!d.locked);
    assert_eq!(d.derived_from, Some("glow@1.0.0".parse().unwrap()));
    let reg2 = packs.register(&base).unwrap();
    let mut net2 = net.clone();
    let (undo, _) = transact(&mut net2, &reg2, edits).unwrap();
    let n = net2.node(g).unwrap();
    assert_eq!(n.type_id, "cmp.pack_glow_local1");
    assert_eq!(
        n.params,
        net.node(g).unwrap().params,
        "values and bindings are kept"
    );
    assert_eq!(net2.wires().count(), net.wires().count());

    // Editing the copy works and changes only the copy.
    let inv = packs
        .edit(
            &copy,
            &base,
            [Edit::SetParam {
                node: NodeId(0),
                param: "gain".into(),
                value: Some(Param::constant(json!(4.0))),
            }],
        )
        .unwrap();
    let reg3 = packs.register(&base).unwrap();
    let flat = packs.flatten(&net2, &reg3).unwrap();
    assert_eq!(
        flat.network
            .node_by_name("glow1__gen1")
            .unwrap()
            .params
            .get("gain"),
        None,
        "seed (promoted, default 1.0) still drives gen.gain"
    );
    // Removing a node a promoted parameter drives is refused and rolled back.
    let err = packs
        .edit(&copy, &base, [Edit::RemoveNode { id: NodeId(1) }])
        .unwrap_err();
    assert!(err.to_string().contains("parameter `amount`"), "{err}");
    packs.edit(&copy, &base, inv).unwrap();
    assert_eq!(packs.get(&copy).unwrap().network, glow().network);

    // Relock returns the instance to the original Pack.
    let (back, notes) = packs.relock(&net2, &reg3, g).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    let mut net3 = net2.clone();
    transact(&mut net3, &reg3, back).unwrap();
    assert_eq!(net3, net);
    // Undo of the unlock edits also restores the original.
    transact(&mut net2, &reg3, undo).unwrap();
    assert_eq!(net2, net);
    assert!(
        packs.unlock(&net, NodeId(0)).is_err(),
        "an LFO is not a Pack"
    );
    let _ = reg;
}

#[test]
fn update_in_place_reports_and_keeps_clamps_or_resets_values() {
    let (mut packs, _, net, [lfo, src, g, _]) = project();
    let base = base();
    let mut net = net;
    let reg = packs.register(&base).unwrap();
    // A second instance with constant overrides, and a binding to nothing.
    let g2 = node(&mut net, &reg, "cmp.pack_glow", "glow2");
    set(&mut net, &reg, g2, "amount", Param::constant(json!(4.5)));
    set(&mut net, &reg, g2, "seed", Param::constant(json!(3.0)));
    connect(&mut net, &reg, src, g2, "in");
    let mut networks = BTreeMap::from([("main".to_owned(), net.clone())]);

    // 1.1.0: amount narrowed to 0…4 (default 1.5), seed removed, a new
    // `tint` parameter, input renamed `src`.
    let mut v2 = glow();
    v2.version = Version::new(1, 1, 0);
    v2.params[0].max = Some(4.0);
    v2.params.remove(1);
    let tint_node = v2
        .network
        .nodes()
        .find(|n| n.type_id == "img.gen")
        .unwrap()
        .id;
    v2.params.push(PromotedParam {
        name: "tint".into(),
        label: String::new(),
        default: json!(1.0),
        min: None,
        max: None,
        help: String::new(),
        targets: vec![ParamTarget::new(tint_node, "gain")],
    });
    v2.inputs[0].name = "src".into();
    let before_packs = packs.clone();
    let report = packs.update(v2.clone(), &base, &mut networks).unwrap();
    let text = report.to_string();
    assert!(
        text.starts_with("glow 1.0.0 → 1.1.0 (compatible)"),
        "{text}"
    );
    for line in [
        "- parameter `seed`",
        "~ parameter `amount`: float 0…5 → float 0…4",
        "+ parameter `tint`",
        "- input `in`",
        "+ input `src`",
        "main/glow1: input `in` disconnected",
        "main/glow2: `amount`: 4.5 clamped to 4.0",
        "main/glow2: `seed` = 3.0 dropped: the parameter was removed",
        "main/glow2: input `in` disconnected",
    ] {
        assert!(text.contains(line), "missing `{line}` in:\n{text}");
    }
    assert!(!report.breaking());
    let main = &networks["main"];
    assert_eq!(
        main.node(g).unwrap().params["amount"],
        Param::bound(json!(2.0), lfo, "lfo"),
        "bindings are kept"
    );
    assert_eq!(main.node(g2).unwrap().params.len(), 1);
    assert_eq!(main.node(g2).unwrap().params["amount"].value, json!(4.0));
    assert!(
        main.wires().all(|w| w.to.node != g && w.to.node != g2),
        "the old input is gone"
    );
    assert_eq!(packs.get("glow").unwrap().version, Version::new(1, 1, 0));
    let reg2 = packs.register(&base).unwrap();
    main.clone().validated(&reg2).unwrap();
    packs.flatten(main, &reg2).unwrap();

    // A failing update changes nothing (the new version is invalid).
    let mut bad = v2;
    bad.version = Version::new(2, 0, 0);
    bad.params[0].targets[0].param = "nope".into();
    let (p, n) = (packs.clone(), networks.clone());
    assert!(packs.update(bad, &base, &mut networks).is_err());
    assert_eq!((packs.clone(), networks.clone()), (p, n));
    assert_ne!(packs, before_packs);

    // A kind change resets; a constant equal to the new default is dropped.
    let mut v3 = packs.get("glow").unwrap().clone();
    v3.version = Version::new(2, 0, 0);
    let pick = node(&mut v3.network, &base, "img.pick", "pick1");
    v3.params[0] = PromotedParam {
        name: "amount".into(),
        label: String::new(),
        default: json!("b"),
        min: None,
        max: None,
        help: String::new(),
        targets: vec![ParamTarget::new(pick, "mode")],
    };
    let report = packs.update(v3, &base, &mut networks).unwrap();
    let text = report.to_string();
    assert!(report.breaking() && text.contains("(breaking)"), "{text}");
    assert!(
        text.contains("~ parameter `amount`: float 0…4 → menu [a, b]"),
        "{text}"
    );
    assert!(
        text.contains("main/glow1: `amount` = binding to n0:lfo reset"),
        "{text}"
    );
    assert!(
        text.contains("main/glow2: `amount` = 4.0 reset to the default \"b\""),
        "{text}"
    );
    assert!(networks["main"].node(g2).unwrap().params.is_empty());
    let _ = src;
}

#[test]
fn update_fixes_instances_inside_other_packs() {
    let reg0 = base();
    let mut packs = set_of([glow()]);
    let reg1 = packs.register(&reg0).unwrap();
    let mut net = Network::default();
    let gi = node(&mut net, &reg1, "cmp.pack_glow", "g1");
    set(&mut net, &reg1, gi, "seed", Param::constant(json!(2.0)));
    let mut holder = PackDef {
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(gi, "out"),
        }],
        network: net,
        ..PackDef::new("holder")
    };
    holder.locked = true;
    packs.insert(holder);
    packs.register(&reg0).unwrap();
    let mut v2 = glow();
    v2.version = Version::new(1, 0, 1);
    v2.params.remove(1);
    let mut nets = BTreeMap::new();
    let report = packs.update(v2, &reg0, &mut nets).unwrap();
    assert!(
        report
            .to_string()
            .contains("pack holder/g1: `seed` = 2.0 dropped"),
        "{report}"
    );
    assert!(
        packs
            .get("holder")
            .unwrap()
            .network
            .node(gi)
            .unwrap()
            .params
            .is_empty()
    );
}

#[test]
fn make_pack_from_a_selection_cooks_the_same_graph() {
    let reg0 = base();
    let mut net = Network::default();
    let lfo = node(&mut net, &reg0, "sig.lfo", "lfo1");
    let src = node(&mut net, &reg0, "img.gen", "src1");
    let a = node(&mut net, &reg0, "img.mix", "mix1");
    let b = node(&mut net, &reg0, "img.mix", "mix2");
    let sink = node(&mut net, &reg0, "img.mix", "out1");
    let ctl = node(&mut net, &reg0, "sig.lfo", "ctl1");
    connect(&mut net, &reg0, src, a, "a");
    connect(&mut net, &reg0, src, b, "b");
    connect(&mut net, &reg0, a, b, "a");
    connect(&mut net, &reg0, b, sink, "a");
    set(
        &mut net,
        &reg0,
        a,
        "gain",
        Param::bound(json!(2.0), lfo, "lfo"),
    );
    set(&mut net, &reg0, b, "gain", Param::constant(json!(3.0)));
    // An outside node bound to ctl1, which goes inside.
    set(
        &mut net,
        &reg0,
        sink,
        "gain",
        Param::bound(json!(1.0), ctl, "lfo"),
    );
    set(
        &mut net,
        &reg0,
        src,
        "gain",
        Param {
            value: json!(1.0),
            expr: Some("op('mix2').par.gain".into()),
            bind: None,
        },
    );
    let cid = net.alloc_comment_id();
    net.apply(
        &reg0,
        Edit::AddComment {
            comment: Comment {
                id: cid,
                text: "c".into(),
                pos: [0.0; 2],
                size: [1.0; 2],
            },
        },
    )
    .unwrap();
    let before_flat = net.clone();

    let selection = BTreeSet::from([a, b, ctl]);
    let inst = net.alloc_id();
    let made = make_pack(
        &net,
        &reg0,
        &selection,
        "duo",
        &[ParamTarget::new(b, "gain")],
        inst,
    )
    .unwrap();
    let d = &made.def;
    assert!(!d.locked && d.version == Version::new(0, 1, 0));
    assert_eq!(d.network.len(), 3);
    assert_eq!(
        d.inputs.len(),
        1,
        "one outside source feeds two inner inputs"
    );
    assert_eq!(d.inputs[0].targets.len(), 2);
    assert_eq!(
        d.outputs
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "out_2"]
    );
    assert_eq!(
        d.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["gain", "mix1_gain"]
    );
    assert_eq!(d.params[0].default, json!(3.0));
    assert_eq!(made.warnings.len(), 1, "{:?}", made.warnings);
    assert!(made.warnings[0].contains("src1.gain"));

    let mut packs = PackSet::default();
    packs.insert(made.def.clone());
    let reg = packs.register(&reg0).unwrap();
    let mut history = History::default();
    let original = net.clone();
    history
        .commit(&mut net, &reg, "make pack", made.edits.clone())
        .unwrap();
    let i = net.node(inst).unwrap();
    assert_eq!(i.name, "duo1");
    assert_eq!(
        i.params["mix1_gain"],
        Param::bound(json!(2.0), lfo, "lfo"),
        "the outside binding moved to the instance"
    );
    assert_eq!(
        net.node(sink).unwrap().params["gain"].bind,
        Some(ChannelRef {
            node: inst,
            channel: "lfo".into()
        })
    );
    net.clone().validated(&reg).unwrap();

    // The flat graph is the original graph, renamed.
    let flat = packs.flatten(&net, &reg).unwrap();
    let canon = |n: &Network, rename: &dyn Fn(&str) -> String| {
        let name = |id: NodeId| rename(&n.node(id).unwrap().name);
        let nodes: BTreeSet<(String, String, String)> = n
            .nodes()
            .map(|x| {
                (
                    rename(&x.name),
                    x.type_id.clone(),
                    serde_json::to_string(
                        &x.params
                            .iter()
                            .map(|(k, p)| {
                                (
                                    k.clone(),
                                    p.value.clone(),
                                    p.bind.as_ref().map(|b| name(b.node)),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap(),
                )
            })
            .collect();
        let wires: BTreeSet<(String, String, String, String)> = n
            .wires()
            .map(|w| {
                (
                    name(w.from.node),
                    w.from.socket,
                    name(w.to.node),
                    w.to.socket,
                )
            })
            .collect();
        (nodes, wires)
    };
    let strip = |s: &str| s.trim_start_matches("duo1__").to_owned();
    assert_eq!(
        canon(&flat.network, &strip),
        canon(&before_flat, &|s: &str| s.to_owned())
    );

    // One undo step restores the original network.
    history.undo(&mut net, &reg).unwrap();
    assert_eq!(net, original);
    assert!(make_pack(&net, &reg0, &BTreeSet::new(), "x", &[], NodeId(99)).is_err());
    assert!(make_pack(&net, &reg0, &selection, "Bad", &[], NodeId(99)).is_err());
}

#[test]
fn expression_references_are_rewritten_precisely() {
    let r = |s: &str| {
        rewrite_refs(s, |n| (n == "a").then(|| "x__a".to_owned())).unwrap_or_else(|| s.to_owned())
    };
    assert_eq!(
        r("op('a').par.x + op(\"a\")['c']"),
        "op('x__a').par.x + op(\"x__a\")['c']"
    );
    assert_eq!(
        r("loop('a') + op('b') + op( 'a' )"),
        "loop('a') + op('b') + op( 'x__a' )"
    );
    assert_eq!(r("op('a"), "op('a");
    assert_eq!(rewrite_refs("1 + 2", |_| None), None);
    assert_eq!(r("é op('a') ✨"), "é op('x__a') ✨");
}

/// Random Pack sets (nesting, dangling and cyclic references, bad
/// interfaces) register, flatten and update with errors, never panics.
#[test]
fn random_pack_sets_never_panic() {
    let reg0 = base();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rng = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n.max(1)
    };
    let types = ["img.gen", "img.mix", "img.feedback", "sig.lfo", "img.pick"];
    let (mut ok, mut failed) = (0, 0);
    for round in 0..400 {
        let mut packs = PackSet::default();
        let n = 1 + rng(6) as usize;
        // A third of the sets are built to be valid (forward references
        // only, matching sockets); the rest are random.
        let clean = round % 3 == 0;
        for p in 0..n {
            let count = 1 + rng(5);
            let mut nodes = Vec::new();
            let mut is_pack = Vec::new();
            for i in 0..count {
                let ty = if clean {
                    if i > 0 && p + 1 < n && rng(3) == 0 {
                        type_id(&format!("q{}", p + 1 + rng((n - p - 1) as u64) as usize))
                    } else {
                        ["img.mix", "img.pick"][rng(2) as usize].to_owned()
                    }
                } else if rng(4) == 0 {
                    type_id(&format!("q{}", rng(n as u64 + 1)))
                } else {
                    types[rng(types.len() as u64) as usize].to_owned()
                };
                is_pack.push(ty.starts_with(TYPE_PREFIX));
                nodes.push(json!({"id": i, "type_id": ty, "name": format!("n{i}"), "pos": [0, 0]}));
            }
            let mut wires = Vec::new();
            for i in 1..count {
                if clean && is_pack[i as usize] {
                    continue;
                }
                let socket = if clean {
                    "a"
                } else {
                    ["a", "b", "in", "prev", "zz"][rng(5) as usize]
                };
                wires.push(json!({"from": {"node": rng(i), "socket": "out"}, "to": {"node": i, "socket": socket}}));
            }
            let net: Network = match serde_json::from_value(
                json!({"next_id": count, "nodes": nodes, "wires": wires}),
            ) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let pick = |rng: &mut dyn FnMut(u64) -> u64| NodeId(rng(count + 1));
            let def = if clean {
                PackDef {
                    params: vec![PromotedParam {
                        name: "gain0".into(),
                        label: String::new(),
                        default: json!(1.0),
                        min: None,
                        max: None,
                        help: String::new(),
                        targets: vec![ParamTarget::new(NodeId(0), "gain")],
                    }],
                    inputs: vec![PackInput {
                        name: "i0".into(),
                        targets: vec![Endpoint::new(NodeId(0), "b")],
                    }],
                    outputs: vec![PackOutput {
                        name: "out".into(),
                        source: Endpoint::new(NodeId(0), "out"),
                    }],
                    network: net,
                    ..PackDef::new(format!("q{p}"))
                }
            } else {
                PackDef {
                    params: (0..rng(3))
                        .map(|k| PromotedParam {
                            name: ["gain", "count", "mode", "Bad"][rng(4) as usize].to_owned()
                                + &k.to_string(),
                            label: String::new(),
                            default: [json!(1.0), json!(3), json!("a"), json!(null)]
                                [rng(4) as usize]
                                .clone(),
                            min: [None, Some(0.0), Some(20.0)][rng(3) as usize],
                            max: None,
                            help: String::new(),
                            targets: vec![ParamTarget::new(
                                pick(&mut rng),
                                ["gain", "count", "mode"][rng(3) as usize],
                            )],
                        })
                        .collect(),
                    inputs: (0..rng(3))
                        .map(|k| PackInput {
                            name: format!("i{k}"),
                            targets: vec![Endpoint::new(
                                pick(&mut rng),
                                ["a", "b", "in", "prev"][rng(4) as usize],
                            )],
                        })
                        .collect(),
                    outputs: (0..rng(3))
                        .map(|k| PackOutput {
                            name: format!("o{k}"),
                            source: Endpoint::new(pick(&mut rng), "out"),
                        })
                        .collect(),
                    network: net,
                    locked: rng(2) == 0,
                    ..PackDef::new(format!("q{p}"))
                }
            };
            packs.insert(def);
        }
        let Ok(reg) = packs.register(&reg0) else {
            failed += 1;
            continue;
        };
        ok += 1;
        let mut top = Network::default();
        for d in packs.iter() {
            let id = top.alloc_id();
            let _ = top.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(id, d.type_id(), format!("t{}", id.0)),
                },
            );
        }
        let _ = packs.flatten(&top, &reg);
        if let Some(d) = packs.iter().next().cloned() {
            let mut nets = BTreeMap::from([("main".to_owned(), top.clone())]);
            let mut v = d;
            v.version = Version::new(9, 0, 0);
            v.params.clear();
            let _ = packs.clone().update(v, &reg0, &mut nets);
        }
    }
    assert!(ok > 20 && failed > 20, "{ok} valid, {failed} invalid");
}
