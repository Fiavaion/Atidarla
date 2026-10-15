//! CLAUDE.md rule 2: SIG cooks allocate nothing after warm-up.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Channels, Params, SigCtx, SigInputs};
use serde_json::json;

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

#[test]
fn every_sig_operator_cooks_without_allocating() {
    let mut reg = Registry::default();
    atd_ops_sig::register(&mut reg);
    let mut net = Network::default();
    let mut sig: HashMap<NodeId, Channels> = HashMap::new();

    // Two upstream sources with fixed channels.
    let source = |net: &mut Network, sig: &mut HashMap<NodeId, Channels>, chans: Channels| {
        let id = net.alloc_id();
        let name = net.unique_name("src");
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.constant", name),
            },
        )
        .unwrap();
        sig.insert(id, chans);
        id
    };
    let a = source(
        &mut net,
        &mut sig,
        Channels::single(&["x", "y"], &[0.25, 0.5]),
    );
    let b = source(&mut net, &mut sig, Channels::single(&["k"], &[2.0]));

    let mut cases = Vec::new();
    for def in atd_ops_sig::DEFS {
        let schema = (def.schema)();
        let id = net.alloc_id();
        let name = net.unique_name("op");
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, schema.type_id.clone(), name),
            },
        )
        .unwrap();
        if schema.type_id == "sig.select" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "channels".into(),
                    value: Some(Param::constant(json!("y x"))),
                },
            )
            .unwrap();
        }
        let inputs: Vec<Option<NodeId>> = (0..schema.inputs.len())
            .map(|i| [Some(a), Some(b)].get(i).copied().flatten())
            .collect();
        cases.push((
            schema.type_id,
            id,
            inputs,
            (def.create)(),
            Channels::default(),
        ));
    }

    let mut run = |frames: usize, t0: f64| {
        for f in 0..frames {
            for (_, id, inputs, op, out) in cases.iter_mut() {
                let mut ctx = SigCtx {
                    inputs: SigInputs::new(inputs, &sig),
                    output: out,
                    params: Params::new(&net, &reg, &sig, *id),
                    time: t0 + f as f64 * 0.016,
                    dt: 0.016,
                    frame: f as u64,
                };
                op.cook(&mut ctx).unwrap();
            }
        }
    };
    let at_start = ALLOCS.load(Ordering::Relaxed);
    run(3, 0.0);
    assert!(
        ALLOCS.load(Ordering::Relaxed) > at_start,
        "the counter sees warm-up allocations"
    );
    let before = ALLOCS.load(Ordering::Relaxed);
    run(100, 1.0);
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocs, 0,
        "SIG cooks allocated {allocs} times over 100 frames"
    );
    for (ty, _, _, _, out) in &cases {
        assert!(!out.is_empty(), "{ty} produced channels");
    }
}
