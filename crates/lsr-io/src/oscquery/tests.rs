use super::*;
use lsr_core::graph::{Edit, Node, Param};
use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema};

fn registry() -> Registry {
    let mut reg = Registry::default();
    reg.register(OperatorSchema {
        type_id: "img.test".into(),
        family: OpFamily::Img,
        label: "Test".into(),
        summary: "test operator".into(),
        version: 1,
        params: vec![
            ParamSchema::float("gain", "Gain", 1.0, Some(0.0), Some(4.0)).help("How much."),
            ParamSchema::int("count", "Count", 3, None, None),
            ParamSchema::toggle("on", "On", true),
            ParamSchema::menu("mode", "Mode", &["a", "b"], "a"),
            ParamSchema::colour("colour", "Colour", [1.0, 0.5, 0.0, 1.0]),
            ParamSchema::string("text", "Text", "hi"),
        ],
        inputs: vec![],
        outputs: vec![],
        capabilities: vec![],
        cook_budget_us: 10,
        time_dependent: false,
        sink: false,
    })
    .unwrap();
    reg
}

fn network(reg: &Registry) -> Network {
    let mut net = Network::default();
    let id = net.alloc_id();
    net.apply(
        reg,
        Edit::AddNode {
            node: Node::new(id, "img.test", "fill1"),
        },
    )
    .unwrap();
    net.apply(
        reg,
        Edit::SetParam {
            node: id,
            param: "gain".into(),
            value: Some(Param::constant(json!(2.5))),
        },
    )
    .unwrap();
    net
}

#[test]
fn the_namespace_has_types_ranges_values_and_access() {
    let reg = registry();
    let net = network(&reg);
    let live = json!(3.0);
    let list = advertise(&net, &reg, |_, p| (p == "gain").then_some(&live));
    assert_eq!(list.len(), 6);
    let root = tree_json(&list, "/").unwrap();
    assert_eq!(root["FULL_PATH"], "/");
    assert_eq!(root["ACCESS"], 0);
    let node = &root["CONTENTS"]["fill1"];
    assert_eq!(node["FULL_PATH"], "/fill1");
    let gain = &node["CONTENTS"]["gain"];
    assert_eq!(
        gain,
        &json!({
            "FULL_PATH": "/fill1/gain",
            "TYPE": "f",
            "ACCESS": 3,
            "VALUE": [3.0],
            "RANGE": [{ "MIN": 0.0, "MAX": 4.0 }],
            "DESCRIPTION": "Gain: How much.",
        }),
        "the live value wins"
    );
    let c = &node["CONTENTS"];
    assert_eq!(c["count"]["TYPE"], "i");
    assert!(c["count"].get("RANGE").is_none(), "unlimited");
    assert_eq!(c["on"]["TYPE"], "T");
    assert_eq!(c["on"]["VALUE"], json!([true]));
    assert_eq!(c["mode"]["RANGE"], json!([{ "VALS": ["a", "b"] }]));
    assert_eq!(c["colour"]["TYPE"], "ffff");
    assert_eq!(c["colour"]["VALUE"], json!([1.0, 0.5, 0.0, 1.0]));
    assert_eq!(c["colour"]["RANGE"].as_array().unwrap().len(), 4);
    assert_eq!(c["text"]["ACCESS"], 1, "text is read-only");

    assert_eq!(tree_json(&list, "/fill1/gain").unwrap(), *gain);
    assert_eq!(tree_json(&list, "/fill1/").unwrap(), *node);
    assert_eq!(tree_json(&list, "/nope"), None);

    let r = routes(&list);
    assert_eq!(r.len(), 5, "everything but text");
    assert_eq!(r[0].address, "/fill1/gain");
}

#[test]
fn queries_answer_one_attribute_or_host_info() {
    let reg = registry();
    let net = network(&reg);
    let list = advertise(&net, &reg, |_, _| None);
    let (status, v) = respond(&list, "Show", 9000, "/fill1/gain?VALUE");
    assert_eq!((status, v), (200, json!({ "VALUE": [2.5] })));
    let (status, _) = respond(&list, "Show", 9000, "/fill1/gain?CONTENTS");
    assert_eq!(status, 204);
    let (status, _) = respond(&list, "Show", 9000, "/missing");
    assert_eq!(status, 404);
    let (status, info) = respond(&list, "Show", 9000, "/?HOST_INFO");
    assert_eq!(status, 200);
    assert_eq!(info["NAME"], "Show");
    assert_eq!(info["OSC_PORT"], 9000);
    assert_eq!(info["OSC_TRANSPORT"], "UDP");
    assert_eq!(info["EXTENSIONS"]["VALUE"], true);
}

#[test]
fn fingerprint_follows_structure_not_values() {
    let reg = registry();
    let mut net = network(&reg);
    let a = fingerprint(&net);
    net.apply(
        &reg,
        Edit::SetParam {
            node: NodeId(0),
            param: "gain".into(),
            value: Some(Param::constant(json!(1.0))),
        },
    )
    .unwrap();
    assert_eq!(fingerprint(&net), a);
    net.apply(
        &reg,
        Edit::Rename {
            node: NodeId(0),
            name: "other".into(),
        },
    )
    .unwrap();
    assert_ne!(fingerprint(&net), a);
}

fn get(port: u16, target: &str) -> (String, String) {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    write!(s, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut text = String::new();
    s.read_to_string(&mut text).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_owned(), body.to_owned())
}

#[test]
fn the_http_server_answers_on_loopback_with_fresh_values() {
    let reg = registry();
    let net = network(&reg);
    let server = OscQueryServer::start(ServerOptions {
        name: "Test show".into(),
        http_port: 0,
        osc_port: 9123,
        loopback: true,
        mdns: false,
    })
    .unwrap();
    // The app's side: publish whenever a request asks.
    let stop = AtomicBool::new(false);
    let list = advertise(&net, &reg, |_, _| None);
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                if server.wants_values() {
                    server.publish(list.clone());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let (status, body) = get(server.port(), "/");
        assert_eq!(status, "HTTP/1.1 200 OK");
        let root: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            root["CONTENTS"]["fill1"]["CONTENTS"]["gain"]["VALUE"],
            json!([2.5])
        );
        let (_, body) = get(server.port(), "/?HOST_INFO");
        let info: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(info["OSC_PORT"], 9123);
        let (status, _) = get(server.port(), "/fill%31/gain?VALUE");
        assert_eq!(status, "HTTP/1.1 200 OK", "percent-decoded");
        let (status, _) = get(server.port(), "/nothing");
        assert_eq!(status, "HTTP/1.1 404 Not Found");
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    });
}

/// mDNS needs multicast on a network interface; some CI sandboxes have
/// none. Registration is checked when the daemon starts; otherwise the
/// reason is printed and the test passes.
#[test]
fn mdns_announces_both_services() {
    let server = OscQueryServer::start(ServerOptions {
        name: "Lasair mDNS test".into(),
        http_port: 0,
        osc_port: 9124,
        loopback: true,
        mdns: true,
    })
    .unwrap();
    if let Some(e) = server.mdns_error() {
        eprintln!("skipped: mDNS unavailable here: {e}");
        return;
    }
    assert!(server.announcing());
    // Another daemon (as a controller app would) resolves the HTTP service.
    let Ok(browser) = mdns_sd::ServiceDaemon::new() else {
        eprintln!("skipped: no second mDNS daemon");
        return;
    };
    let events = browser.browse(HTTP_SERVICE).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut found = None;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info))
                if info.get_fullname().starts_with("Lasair mDNS test") =>
            {
                found = Some(info.get_port());
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = browser.shutdown();
    match found {
        Some(port) => assert_eq!(port, server.port()),
        None => eprintln!("skipped: registered, but not resolved within 5 s (no multicast route?)"),
    }
}
