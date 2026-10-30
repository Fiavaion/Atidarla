//! Cache keys of work items (FR-B3).
//!
//! The key is a SHA-256 over a canonical text of everything the target's
//! image depends on that the project says: the nodes upstream of the
//! target (through wires and channel bindings) in ID order, each with its
//! type, its inputs, and every parameter of its schema with its stored
//! value, expression and binding, or the schema default; the checksums of
//! the project's pinned models those nodes name; the frame, frame rate and
//! output size. Node names and canvas positions are left out, and so are
//! placement parameters (an AI node's `route`, a ComfyUI node's `server`):
//! where an item cooks does not change what it is.
//!
//! Not covered: the contents of files nodes read (movies, project shaders,
//! kernels, workflows), expressions that read other nodes by name, and
//! model weights not pinned in `project.toml`. `atidarla batch run --force`
//! cooks again regardless.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use atd_core::graph::{Network, NodeId};
use atd_core::registry::Registry;
use sha2::{Digest, Sha256};

/// Changes when the key's text changes, so old bins do not match.
const VERSION: &str = "atidarla batch key 1";

/// Parameters that say where a node runs, not what it makes.
pub fn is_placement(type_id: &str, param: &str) -> bool {
    (type_id.starts_with("ai.") && matches!(param, "route" | "in_flight" | "budget_ms"))
        || (type_id.starts_with("comfy.") && matches!(param, "server" | "run" | "auto_run"))
}

/// What a key is computed over.
#[derive(Debug, Clone, Copy)]
pub struct KeyInput<'a> {
    /// The network the engine cooks (Packs expanded), with the item's
    /// overrides applied.
    pub net: &'a Network,
    pub registry: &'a Registry,
    pub target: NodeId,
    pub frame: u64,
    pub fps: f64,
    pub size: [u32; 2],
    /// The project's pinned models: ID → SHA-256.
    pub models: &'a BTreeMap<String, String>,
}

/// The nodes `target` depends on, itself included, in ID order.
pub fn upstream(net: &Network, target: NodeId) -> BTreeSet<NodeId> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![target];
    while let Some(id) = todo.pop() {
        if net.node(id).is_none() || !seen.insert(id) {
            continue;
        }
        todo.extend(net.inputs_of(id).map(|(_, from)| from.node));
        todo.extend(net.bindings_of(id).map(|b| b.node));
    }
    seen
}

/// The canonical text the key hashes (exposed for tests and debugging).
pub fn key_text(k: &KeyInput<'_>) -> String {
    let mut s = String::with_capacity(1024);
    let _ = writeln!(s, "{VERSION}");
    let mut models = BTreeSet::new();
    for id in upstream(k.net, k.target) {
        let node = k.net.node(id).expect("upstream nodes exist");
        let _ = writeln!(s, "node {} {}", id.0, node.type_id);
        for (socket, from) in k.net.inputs_of(id) {
            let _ = writeln!(s, "  in {socket} <- {}.{}", from.node.0, from.socket);
        }
        let schema = k.registry.get(&node.type_id);
        let mut names: BTreeSet<&str> = node.params.keys().map(String::as_str).collect();
        if let Some(sc) = schema {
            names.extend(sc.params.iter().map(|p| p.name.as_str()));
        }
        for name in names {
            if is_placement(&node.type_id, name) {
                continue;
            }
            // A constant is its value, whether stored or the default; an
            // expression or a binding is written whole.
            let json = |v: &serde_json::Value| serde_json::to_string(v).unwrap_or_default();
            let value = match node.params.get(name) {
                Some(p) if p.expr.is_none() && p.bind.is_none() => json(&p.value),
                Some(p) => serde_json::to_string(p).unwrap_or_default(),
                None => schema
                    .and_then(|sc| sc.params.iter().find(|p| p.name == name))
                    .map(|p| json(&p.default))
                    .unwrap_or_default(),
            };
            let _ = writeln!(s, "  {name} = {value}");
            if name == "model"
                && let Some(m) = k
                    .net
                    .param_value(k.registry, id, name)
                    .and_then(|v| v.as_str())
                && let Some(sha) = k.models.get(m)
            {
                models.insert((m.to_owned(), sha.clone()));
            }
        }
    }
    for (id, sha) in models {
        let _ = writeln!(s, "model {id} {sha}");
    }
    let _ = writeln!(
        s,
        "target {} frame {} fps {:?} size {}x{}",
        k.target.0, k.frame, k.fps, k.size[0], k.size[1]
    );
    s
}

/// The item's cache key: 64 lower-case hex digits.
pub fn cache_key(k: &KeyInput<'_>) -> String {
    let digest = Sha256::digest(key_text(k).as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::graph::{Edit, Endpoint, Node, Param, Wire};
    use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
    use serde_json::json;

    fn registry() -> Registry {
        let mut r = Registry::default();
        let socket = |name: &str| SocketSchema {
            name: name.into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        };
        let img = |id: &str, inputs: usize, params: Vec<ParamSchema>| OperatorSchema {
            type_id: id.into(),
            family: if id.starts_with("ai.") {
                OpFamily::Ai
            } else {
                OpFamily::Img
            },
            label: id.into(),
            summary: id.into(),
            version: 1,
            params,
            inputs: (0..inputs).map(|i| socket(&format!("in{i}"))).collect(),
            outputs: vec![socket("out")],
            capabilities: Vec::new(),
            cook_budget_us: 1,
            time_dependent: false,
            sink: false,
        };
        r.register(img(
            "img.src",
            0,
            vec![ParamSchema::float("level", "Level", 0.5, None, None)],
        ))
        .unwrap();
        r.register(img(
            "ai.gen",
            1,
            vec![
                ParamSchema::string("prompt", "Prompt", "a cat"),
                ParamSchema::string("route", "Route", "local"),
                ParamSchema::string("model", "Model", "m1"),
            ],
        ))
        .unwrap();
        r
    }

    /// src(1) -> gen(2); other(3) is not upstream.
    fn net(reg: &Registry) -> Network {
        let mut n = Network::default();
        for (id, ty, name) in [
            (1, "img.src", "src"),
            (2, "ai.gen", "gen"),
            (3, "img.src", "other"),
        ] {
            n.apply(
                reg,
                Edit::AddNode {
                    node: Node::new(NodeId(id), ty, name),
                },
            )
            .unwrap();
        }
        n.apply(
            reg,
            Edit::Connect {
                wire: Wire::new(
                    Endpoint::new(NodeId(1), "out"),
                    Endpoint::new(NodeId(2), "in0"),
                ),
            },
        )
        .unwrap();
        n
    }

    fn set(n: &mut Network, reg: &Registry, node: u64, param: &str, v: serde_json::Value) {
        n.apply(
            reg,
            Edit::SetParam {
                node: NodeId(node),
                param: param.into(),
                value: Some(Param::constant(v)),
            },
        )
        .unwrap();
    }

    fn key(n: &Network, reg: &Registry, models: &BTreeMap<String, String>) -> String {
        cache_key(&KeyInput {
            net: n,
            registry: reg,
            target: NodeId(2),
            frame: 0,
            fps: 60.0,
            size: [64, 64],
            models,
        })
    }

    #[test]
    fn keys_follow_what_the_target_depends_on() {
        let reg = registry();
        let none = BTreeMap::new();
        let base = net(&reg);
        let k0 = key(&base, &reg, &none);
        assert_eq!(k0.len(), 64);
        assert_eq!(key(&net(&reg), &reg, &none), k0, "deterministic");

        // Not upstream, placement, names and positions: same key.
        let mut n = base.clone();
        set(&mut n, &reg, 3, "level", json!(0.9));
        set(&mut n, &reg, 2, "route", json!("lan:a"));
        n.apply(
            &reg,
            Edit::Rename {
                node: NodeId(1),
                name: "renamed".into(),
            },
        )
        .unwrap();
        n.apply(
            &reg,
            Edit::Move {
                node: NodeId(2),
                pos: [10.0, 5.0],
            },
        )
        .unwrap();
        assert_eq!(key(&n, &reg, &none), k0);

        // Upstream values, a default written out, and frame/size: new keys.
        let mut n = base.clone();
        set(&mut n, &reg, 1, "level", json!(0.9));
        assert_ne!(key(&n, &reg, &none), k0);
        let mut n = base.clone();
        set(&mut n, &reg, 2, "prompt", json!("a dog"));
        assert_ne!(key(&n, &reg, &none), k0);
        let mut n = base.clone();
        set(&mut n, &reg, 2, "prompt", json!("a cat"));
        assert_eq!(key(&n, &reg, &none), k0, "a stored default is the default");
        let k = |frame, size| {
            cache_key(&KeyInput {
                net: &base,
                registry: &reg,
                target: NodeId(2),
                frame,
                fps: 60.0,
                size,
                models: &none,
            })
        };
        assert_ne!(k(1, [64, 64]), k0);
        assert_ne!(k(0, [64, 32]), k0);

        // A pinned model's checksum counts once it is named.
        let pinned = BTreeMap::from([("m1".to_owned(), "ab".repeat(32))]);
        assert_ne!(key(&base, &reg, &pinned), k0);
        let other = BTreeMap::from([("m2".to_owned(), "ab".repeat(32))]);
        assert_eq!(key(&base, &reg, &other), k0);
    }

    #[test]
    fn placement_parameters_are_route_and_server() {
        assert!(is_placement("ai.diffusion", "route"));
        assert!(is_placement("comfy.invert", "server"));
        assert!(!is_placement("ai.diffusion", "seed"));
        assert!(!is_placement("img.constant", "route"));
    }
}
