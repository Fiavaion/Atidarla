//! CLAUDE.md rule 2: the per-frame cook plan allocates nothing after warm-up.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use atd_core::cook::Scheduler;
use atd_core::graph::{Edit, Endpoint, Network, Node, Wire};
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

#[test]
fn steady_state_plan_does_not_allocate() {
    let mut reg = Registry::default();
    reg.register(schema("img.src", vec![], true)).unwrap();
    reg.register(schema("img.mix", vec![socket("a"), socket("b")], false))
        .unwrap();

    // A 200-node graph: time-dependent sources feeding a chain of mixes.
    let mut net = Network::default();
    let mut prev = None;
    let mut last = None;
    for i in 0..100 {
        let src = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(src, "img.src", format!("src{i}")),
            },
        )
        .unwrap();
        let mix = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(mix, "img.mix", format!("mix{i}")),
            },
        )
        .unwrap();
        let wire = |from, input: &str| Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(mix, input)),
        };
        net.apply(&reg, wire(src, "a")).unwrap();
        if let Some(p) = prev {
            net.apply(&reg, wire(p, "b")).unwrap();
        }
        prev = Some(mix);
        last = Some(mix);
    }
    let requested = [last.unwrap()];

    let mut s = Scheduler::default();
    let mut plan = Vec::new();
    // Warm-up frames grow buffers and create stats entries.
    let at_start = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..3 {
        s.plan(&net, &reg, &requested, &mut plan).unwrap();
        for &n in &plan {
            s.record(n, std::time::Duration::from_micros(5));
        }
    }
    assert_eq!(
        plan.len(),
        200,
        "every node depends on a time-dependent source"
    );
    assert!(
        ALLOCS.load(Ordering::Relaxed) > at_start,
        "the counter sees warm-up allocations"
    );

    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        s.plan(&net, &reg, &requested, &mut plan).unwrap();
        for &n in &plan {
            s.record(n, std::time::Duration::from_micros(5));
        }
    }
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocs, 0,
        "plan + record allocated {allocs} times over 100 frames"
    );
}
