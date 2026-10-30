//! CLAUDE.md rule 2: once bound, a timeline evaluates and fires cues
//! without allocating.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use atd_core::graph::{Edit, Network, Node, NodeId};
use atd_core::registry::Registry;
use atd_format::timeline::{Cue, CueAction, Interp, TimelineDoc};
use atd_timeline::{Signals, Timeline, Transport};
use serde_json::json;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

struct Beat;
impl Signals for Beat {
    fn channel(&self, _: NodeId, _: &str) -> Option<f64> {
        Some(0.75)
    }
    fn param(&self, _: NodeId, _: &str, index: Option<usize>) -> Option<f64> {
        Some(index.map_or(0.5, |i| i as f64 / 4.0))
    }
}

#[test]
fn a_bound_timeline_plays_without_allocating() {
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut net = Network::default();
    for (ty, name) in [
        ("img.level", "level1"),
        ("img.kaleidoscope", "kaleido1"),
        ("img.constant", "constant1"),
        ("sig.lfo", "lfo1"),
    ] {
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, ty, name),
            },
        )
        .unwrap();
    }
    let mut doc = TimelineDoc::default();
    for (node, param, comp) in [
        (0, "brightness", None),
        (0, "invert", None),
        (1, "segments", None),
        (2, "colour", Some(0)),
        (2, "colour", Some(2)),
    ] {
        let t = doc.track_mut(NodeId(node), param, comp);
        for k in 0..20 {
            let i = t.set_key(f64::from(k) * 0.5, f64::from(k % 5));
            t.keys[i].interp = Interp::ALL[k as usize % 3];
        }
    }
    for id in 0..50u64 {
        doc.cues.push(Cue {
            id,
            name: String::new(),
            t: (id % 5 != 0).then_some(id as f64 * 0.2),
            when: (id % 3 == 0).then(|| "op('lfo1')['lfo'] > 0.5 and time > 1".to_owned()),
            actions: vec![
                CueAction::Live {
                    node: NodeId(0),
                    param: "gamma".into(),
                    value: json!(id as f64),
                },
                CueAction::Live {
                    node: NodeId(2),
                    param: "colour".into(),
                    value: json!([1.0, 0.5, 0.25, 1.0]),
                },
            ],
        });
    }
    let mut tl = Timeline::bind(doc, &net, &reg);
    assert!(tl.problems().is_empty(), "{:?}", tl.problems());
    let mut transport = Transport::new(12.0);
    transport.loop_region = Some([2.0, 9.0]);
    transport.looping = true;

    let run = |tl: &mut Timeline, transport: &mut Transport, frames: u32| {
        let mut fired = 0;
        transport.play();
        for f in 0..frames {
            let now = f64::from(f) / 60.0;
            if f % 97 == 0 {
                tl.go();
            }
            if f % 400 == 399 {
                transport.seek(3.0, false);
            }
            tl.begin();
            match transport.advance(now) {
                atd_timeline::Motion::Forward {
                    from,
                    to,
                    include_from,
                } => tl.cross(from, to, include_from),
                atd_timeline::Motion::Wrapped {
                    from,
                    end,
                    start,
                    to,
                } => {
                    tl.cross(from, end, false);
                    tl.rearm(start, end, true);
                    tl.cross(start, to, true);
                }
                atd_timeline::Motion::Jumped { to, .. } => tl.rearm(to, f64::INFINITY, true),
                atd_timeline::Motion::Still => {}
            }
            let t = transport.time();
            fired += tl.finish(t, &Beat).len();
            tl.evaluate(t, &Beat);
        }
        tl.reset();
        fired
    };
    // Warm up: one full pass.
    run(&mut tl, &mut transport, 1200);
    transport.stop();
    ALLOCS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    let fired = run(&mut tl, &mut transport, 1200);
    tl.chase(7.0);
    tl.evaluate(7.0, &Beat);
    ON.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "allocations while playing"
    );
    assert!(fired > 50, "cues fired: {fired}");
}
