//! A [`Player`] on an engine. CLAUDE.md rule 2 for the live path: a
//! playing player setting number and colour live values through
//! `Engine::set_live` allocates nothing after warm-up. The engine's own
//! frame cook (wgpu records passes) is measured separately and not part of
//! the claim, as in the IMG and PNT allocation tests. Stopping releases
//! only the live values the player still owns.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_format::timeline::TimelineDoc;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_timeline::Player;
use serde_json::json;

/// Counts allocations on the thread that turned counting on.
struct Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}
fn tally() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
    }
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        tally();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        tally();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn count(f: impl FnOnce()) -> usize {
    ALLOCS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCS.with(Cell::get)
}

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 32,
        height: 16,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name(ty.split('.').nth(1).unwrap());
    e.edit(
        "add",
        [Edit::AddNode {
            node: Node::new(id, ty, name),
        }],
    )
    .unwrap();
    id
}

#[test]
fn playing_tracks_set_live_values_without_allocating() {
    let Some(mut e) = engine() else {
        return;
    };
    let colour = add(&mut e, "img.constant");
    let level = add(&mut e, "img.level");
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(colour, "out"), Endpoint::new(level, "in")),
        }],
    )
    .unwrap();

    let mut doc = TimelineDoc {
        length: 100.0,
        ..TimelineDoc::default()
    };
    for (node, param, component) in [
        (level, "brightness", None),
        (colour, "colour", Some(0)),
        (colour, "colour", Some(2)),
    ] {
        let t = doc.track_mut(node, param, component);
        t.set_key(0.0, 0.0);
        t.set_key(100.0, 1.0);
    }
    let mut p = Player::new(doc, &e);
    p.transport.play();
    let frame = |f: u32| f64::from(f) / 60.0;
    for f in 0..60 {
        p.tick(&mut e, frame(f));
        e.cook_frame(Some(level), frame(f)).unwrap();
    }

    // Every frame changes all three values: the player and set_live add
    // nothing.
    let mut ticks = 0;
    for f in 60..180 {
        ticks += count(|| p.tick(&mut e, frame(f)));
        e.cook_frame(Some(level), frame(f)).unwrap();
    }
    let live = e.live_values();
    assert!(live.get(level, "brightness").is_some());
    assert!(live.get(colour, "colour").is_some());
    // The first value of a parameter is stored (warm-up); changes are not.
    e.set_live(level, "gamma", Some(&json!(1.0)));
    let set = count(|| {
        for f in 1..61 {
            e.set_live(level, "gamma", Some(&json!(1.0 + f64::from(f) * 0.01)));
        }
    });
    let cook = count(|| {
        for f in 180..240 {
            e.cook_frame(Some(level), frame(f)).unwrap();
        }
    });
    eprintln!(
        "allocations: Player::tick x120 {ticks}, Engine::set_live x60 {set}, \
         cook_frame x60 (engine and wgpu, not claimed) {cook}"
    );
    assert_eq!(ticks, 0, "Player::tick with set_live, 120 frames");
    assert_eq!(set, 0, "Engine::set_live of a number, 60 changes");
}

/// The components of a vector track that are not keyed follow the
/// parameter's expression (PRD A66), not its stored value.
#[test]
fn unkeyed_components_follow_a_driven_colour() {
    let Some(mut e) = engine() else {
        return;
    };
    let colour = add(&mut e, "img.constant");
    e.edit(
        "expr",
        [Edit::SetParam {
            node: colour,
            param: "colour".into(),
            value: Some(Param {
                value: json!([0.0, 0.0, 0.0, 0.75]),
                expr: Some("0.25 + 0.25".into()),
                bind: None,
            }),
        }],
    )
    .unwrap();
    e.cook_frame(Some(colour), 0.0).unwrap();
    let mut doc = TimelineDoc {
        length: 10.0,
        ..TimelineDoc::default()
    };
    let t = doc.track_mut(colour, "colour", Some(0));
    t.set_key(0.0, 1.0);
    t.set_key(10.0, 1.0);
    let mut p = Player::new(doc, &e);
    p.transport.play();
    p.tick(&mut e, 0.0);
    // Red from the track; green and blue from the expression; the stored
    // alpha.
    assert_eq!(
        e.live_values().get(colour, "colour"),
        Some(&json!([1.0, 0.5, 0.5, 0.75]))
    );
}

/// Stop clears what the timeline set, but not a value another writer (a
/// mapping) set on the same parameter since.
#[test]
fn stop_leaves_values_another_writer_set() {
    let Some(mut e) = engine() else {
        return;
    };
    let level = add(&mut e, "img.level");
    let mut doc = TimelineDoc {
        length: 10.0,
        ..TimelineDoc::default()
    };
    for param in ["brightness", "gamma"] {
        let t = doc.track_mut(level, param, None);
        t.set_key(0.0, 0.5);
        t.set_key(10.0, 1.0);
    }
    let mut p = Player::new(doc, &e);
    p.transport.play();
    p.tick(&mut e, 0.0);
    p.tick(&mut e, 0.5);
    assert!(e.live_values().get(level, "brightness").is_some());
    assert!(e.live_values().get(level, "gamma").is_some());
    let mapped = json!(0.125);
    e.set_live(level, "gamma", Some(&mapped));
    p.stop(&mut e);
    assert_eq!(e.live_values().get(level, "brightness"), None, "released");
    assert_eq!(
        e.live_values().get(level, "gamma"),
        Some(&mapped),
        "the mapping's value stays"
    );
}

/// The document's `[clock]` and `stage` mode set up the player, also
/// after a rebind; offline renders always drive the Stage.
#[test]
fn the_document_sets_the_clock_and_stage_mode() {
    use atd_format::timeline::TimecodeClock;
    use atd_timeline::ClockSource;
    let Some(mut e) = engine() else {
        return;
    };
    let tc = add(&mut e, "sig.timecode_in");
    let doc = TimelineDoc {
        follow_stage: true,
        clock: Some(TimecodeClock {
            node: tc,
            offset: 2.5,
        }),
        ..TimelineDoc::default()
    };
    let mut p = Player::new(doc.clone(), &e);
    assert_eq!(p.transport.clock, ClockSource::Timecode);
    assert_eq!(p.timecode_node(), Some(tc));
    assert_eq!(p.transport.chase.config.offset, 2.5);
    assert!(p.timeline().follow_stage());
    assert!(p.watched().contains(&tc), "the chased node cooks");

    p.rebind(TimelineDoc::default(), &mut e);
    assert_eq!(p.transport.clock, ClockSource::Internal);
    assert_eq!(p.timecode_node(), None);
    assert!(!p.timeline().follow_stage());

    let offline = Player::offline(doc, &e);
    assert!(
        !offline.timeline().follow_stage(),
        "offline drives the Stage"
    );
}

/// Conditions read numbers as their expression or binding drives them,
/// as they read a vector's components; live values are left out.
#[test]
fn conditions_read_driven_numbers() {
    use atd_timeline::{EngineSignals, Signals};
    let Some(mut e) = engine() else {
        return;
    };
    let level = add(&mut e, "img.level");
    let colour = add(&mut e, "img.constant");
    for (node, param, value) in [
        (level, "brightness", json!(1.0)),
        (colour, "colour", json!([0.0, 0.0, 0.0, 1.0])),
    ] {
        e.edit(
            "expr",
            [Edit::SetParam {
                node,
                param: param.into(),
                value: Some(Param {
                    value,
                    expr: Some("0.25 + 0.5".into()),
                    bind: None,
                }),
            }],
        )
        .unwrap();
    }
    e.cook_frame(Some(level), 0.0).unwrap();
    e.cook_frame(Some(colour), 0.0).unwrap();
    e.set_live(level, "brightness", Some(&json!(0.125)));
    let s = EngineSignals(&e);
    assert_eq!(s.param(level, "brightness", None), Some(0.75), "driven");
    assert_eq!(s.param(colour, "colour", Some(1)), Some(0.75), "driven");
    assert_eq!(s.param(colour, "colour", Some(3)), Some(1.0), "stored");
    assert_eq!(s.param(colour, "colour", None), None, "not a number");
    assert_eq!(s.param(level, "gamma", None), Some(1.0), "stored");
}
