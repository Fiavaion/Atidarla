//! CLAUDE.md rule 2: a vector an OSCQuery route holds allocates nothing
//! per frame while its other components follow their expression.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use lsr_core::graph::{NodeId, assign_value};
use lsr_core::registry::ParamKind;
use lsr_format::mappings::Mappings;
use lsr_io::mapping::{MappingRuntime, Route};
use serde_json::{Value, json};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Only this test's frames are measured, not other tests' threads.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn counted() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counted() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counted() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const N: NodeId = NodeId(1);

/// One frame: the runtime's update, its writes applied as live values.
/// Only the runtime is counted; `current` stands in for the engine's value.
fn frame(rt: &mut MappingRuntime, kind: &ParamKind, live: &mut Value, t: f64) -> usize {
    let current = live.clone();
    COUNTING.with(|c| c.set(true));
    rt.update(
        &Mappings::default(),
        |n, p| (n == N && p == "scale").then_some((kind, &current)),
        |_, _, i| (i > 0).then(|| (t + i as f64).sin()),
    );
    let mut n = 0;
    for (_, _, v) in rt.writes() {
        assign_value(live, v);
        n += 1;
    }
    COUNTING.with(|c| c.set(false));
    n
}

#[test]
fn a_held_route_allocates_nothing_per_frame() {
    let kind = ParamKind::Vec { n: 3 };
    let mut rt = MappingRuntime::new();
    rt.set_routes(vec![Route {
        address: "/s".into(),
        node: N,
        param: "scale".into(),
        kind: kind.clone(),
    }]);
    let mut live = json!([1.0, 1.0, 1.0]);
    rt.hub().push_osc("/s", &[0.5]);
    assert_eq!(frame(&mut rt, &kind, &mut live, 0.0), 1);
    assert_eq!(frame(&mut rt, &kind, &mut live, 1.0), 1, "warm-up");
    ALLOCS.store(0, Ordering::Relaxed);
    for f in 2..100 {
        assert_eq!(
            frame(&mut rt, &kind, &mut live, f64::from(f)),
            1,
            "frame {f}: [1] and [2] follow the expression"
        );
        assert_eq!(live[0], json!(0.5), "frame {f}: OSC holds [0]");
    }
    assert_eq!(ALLOCS.load(Ordering::Relaxed), 0, "allocations per frame");
}
