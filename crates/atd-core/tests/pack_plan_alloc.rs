//! CLAUDE.md rule 2 with Packs: flattening happens when a network is loaded
//! or edited; planning and cooking the flat network allocates nothing after
//! warm-up, and its order is the same every frame and every run.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use atd_core::cook::Scheduler;
use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Wire};
use atd_core::pack::{PackDef, PackInput, PackOutput, PackSet};
use atd_core::registry::{OpFamily, OperatorSchema, Registry, SocketSchema, WireType};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn socket(name: &str) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback: false,
    }
}

fn schema(type_id: &str, inputs: Vec<SocketSchema>, time_dependent: bool) -> OperatorSchema {
    OperatorSchema {
        type_id: type_id.into(),
        family: OpFamily::Img,
        label: type_id.into(),
        summary: String::new(),
        version: 1,
        params: vec![],
        inputs,
        outputs: vec![socket("out")],
        capabilities: vec![],
        cook_budget_us: 10,
        time_dependent,
        sink: false,
    }
}

fn add(net: &mut Network, reg: &Registry, ty: &str, name: &str) -> NodeId {
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

fn wire(net: &mut Network, reg: &Registry, from: NodeId, to: NodeId, input: &str) {
    net.apply(
        reg,
        Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
        },
    )
    .unwrap();
}

#[test]
fn planning_a_flattened_network_does_not_allocate() {
    let mut base = Registry::default();
    base.register(schema("img.src", vec![], true)).unwrap();
    base.register(schema("img.mix", vec![socket("a"), socket("b")], false))
        .unwrap();

    // `stage`: in → mix.a, src → mix.b; three of them nested in `chain`.
    let mut inner = Network::default();
    let src = add(&mut inner, &base, "img.src", "src1");
    let mix = add(&mut inner, &base, "img.mix", "mix1");
    wire(&mut inner, &base, src, mix, "b");
    let mut packs = PackSet::default();
    packs.insert(PackDef {
        inputs: vec![PackInput {
            name: "in".into(),
            targets: vec![Endpoint::new(mix, "a")],
        }],
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(mix, "out"),
        }],
        network: inner,
        ..PackDef::new("stage")
    });
    let reg = packs.register(&base).unwrap();
    let mut chain = Network::default();
    let mut prev = None;
    for i in 0..3 {
        let s = add(&mut chain, &reg, "cmp.pack_stage", &format!("stage{i}"));
        if let Some(p) = prev {
            wire(&mut chain, &reg, p, s, "in");
        }
        prev = Some(s);
    }
    packs.insert(PackDef {
        outputs: vec![PackOutput {
            name: "out".into(),
            source: Endpoint::new(prev.unwrap(), "out"),
        }],
        network: chain,
        ..PackDef::new("chain")
    });
    let reg = packs.register(&base).unwrap();

    // The top level: 30 chains in series (180 flat nodes).
    let mut top = Network::default();
    let mut last = None;
    for i in 0..30 {
        let c = add(&mut top, &reg, "cmp.pack_chain", &format!("chain{i}"));
        if let Some(p) = last {
            // A mix outside joins consecutive chains.
            let inner_in = top.alloc_id();
            top.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(inner_in, "img.mix", format!("join{i}")),
                },
            )
            .unwrap();
            wire(&mut top, &reg, p, inner_in, "a");
            wire(&mut top, &reg, c, inner_in, "b");
            last = Some(inner_in);
        } else {
            last = Some(c);
        }
    }
    let flat = packs.flatten(&top, &reg).unwrap();
    assert_eq!(flat.network.len(), 30 * 6 + 29);
    let requested = [flat.view(last.unwrap()).unwrap()];
    assert_eq!(
        packs.flatten(&top, &reg).unwrap(),
        flat,
        "flattening is deterministic"
    );

    let mut s = Scheduler::default();
    let mut plan = Vec::new();
    for _ in 0..3 {
        s.plan(&flat.network, &base, &requested, &mut plan).unwrap();
        for &n in &plan {
            s.record(n, std::time::Duration::from_micros(5));
        }
    }
    let first = plan.clone();
    assert_eq!(
        first.len(),
        flat.network.len(),
        "every node is time-dependent or downstream of one"
    );

    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        s.plan(&flat.network, &base, &requested, &mut plan).unwrap();
        for &n in &plan {
            s.record(n, std::time::Duration::from_micros(5));
        }
    }
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocs, 0,
        "plan + record allocated {allocs} times over 100 frames"
    );
    assert_eq!(plan, first, "the cook order is the same every frame");
}
