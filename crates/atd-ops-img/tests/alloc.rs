//! Rule 2 for IMG cooks: steady-state frames create no GPU objects (bind
//! groups, views) and allocate little. wgpu itself allocates while
//! recording commands, so the test bounds allocations per node per frame
//! rather than demanding zero.

use std::alloc::{GlobalAlloc, Layout, System};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use serde_json::json;

struct Counting;
thread_local! {
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Per thread, so tests running in parallel do not count each other.
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn allocs() -> usize {
    ALLOCS.with(std::cell::Cell::get)
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn img_chain_steady_state_allocations() {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);

    // A time-dependent source so every node cooks every frame.
    let chain = [
        "img.noise",
        "img.blur",
        "img.level",
        "img.hsv_adjust",
        "img.transform",
        "img.kaleidoscope",
        "img.mirror",
        "img.edge",
        "img.composite",
        "img.feedback",
    ];
    let mut ids: Vec<NodeId> = Vec::new();
    let mut edits = Vec::new();
    for ty in chain {
        let id = e.alloc_id();
        edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", ids.len())),
        });
        if let Some(prev) = ids.last() {
            let input = if ty == "img.composite" { "a" } else { "in" };
            edits.push(Edit::Connect {
                wire: Wire::new(Endpoint::new(*prev, "out"), Endpoint::new(id, input)),
            });
        }
        ids.push(id);
    }
    // An LFO on the noise's z makes the whole chain change every frame.
    let lfo = e.alloc_id();
    edits.push(Edit::AddNode {
        node: Node::new(lfo, "sig.lfo", "lfo1"),
    });
    edits.push(Edit::SetParam {
        node: ids[0],
        param: "z".into(),
        value: Some(Param::bound(json!(0.0), lfo, "lfo")),
    });
    e.edit("chain", edits).unwrap();
    let out = *ids.last().unwrap();

    let mut t = 0.0;
    for _ in 0..30 {
        e.cook(&[out], t).unwrap();
        t += 1.0 / 60.0;
    }
    let frames = 100;
    COUNTING.with(|c| c.set(true));
    let before = allocs();
    for _ in 0..frames {
        let cooked = e.cook(&[out], t).unwrap();
        assert_eq!(cooked, ids.len() + 1, "every node cooks every frame");
        t += 1.0 / 60.0;
    }
    let allocs = allocs() - before;
    COUNTING.with(|c| c.set(false));
    let per_node = allocs as f64 / (frames * ids.len()) as f64;
    eprintln!(
        "IMG steady state: {allocs} allocations over {frames} frames, {per_node:.1} per node-cook"
    );
    assert!(
        per_node <= BOUND,
        "{per_node:.1} allocations per node-cook (bound {BOUND})"
    );
}

/// Allocations per node-cook allowed: what remains is wgpu recording the
/// render pass (17.7 measured on lavapipe; 70 before bind-group caching).
const BOUND: f64 = 25.0;

const ISF_FILTER: &str = r#"/*{
    "INPUTS": [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "amount", "TYPE": "float", "DEFAULT": 0.5}
    ]
}*/
void main() {
    gl_FragColor = IMG_THIS_PIXEL(inputImage) * amount;
}
"#;

const ISF_PASSES: &str = r#"/*{
    "INPUTS": [{"NAME": "inputImage", "TYPE": "image"}],
    "PASSES": [
        {"TARGET": "acc", "PERSISTENT": true, "FLOAT": true},
        {"TARGET": "small", "WIDTH": "$WIDTH/4.0", "HEIGHT": "$HEIGHT/4.0"},
        {}
    ]
}*/
void main() {
    if (PASSINDEX == 0) {
        gl_FragColor = mix(IMG_THIS_PIXEL(acc), IMG_THIS_PIXEL(inputImage), 0.1);
    } else if (PASSINDEX == 1) {
        gl_FragColor = IMG_THIS_PIXEL(acc);
    } else {
        gl_FragColor = IMG_NORM_PIXEL(small, isf_FragNormCoord) + IMG_THIS_PIXEL(acc);
    }
}
"#;

/// Every IMG operator (and two ISF shaders) after a changing source, one at
/// a time: allocations per frame beyond the source's own. wgpu allocates
/// while recording each render pass, and how much depends on the backend,
/// so bounds are relative to the typical operator (the median, one pass):
/// an operator that creates bind groups or views every frame stands out.
/// Multi-pass operators get an allowance for their pass count.
#[test]
fn every_img_operator_steady_state_allocations() {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    atd_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    atd_ops_sig::library(&mut lib);
    let isf = vec![
        atd_ops_img::isf::IsfDef::new("Alloc Filter", ISF_FILTER, None).unwrap(),
        atd_ops_img::isf::IsfDef::new("Alloc Passes", ISF_PASSES, None).unwrap(),
    ];
    atd_ops_img::isf::register(isf, &mut reg, &mut lib);
    let types: Vec<(String, Option<String>)> = reg
        .iter()
        .filter(|s| s.family == atd_core::registry::OpFamily::Img)
        .map(|s| {
            let input = s
                .inputs
                .iter()
                .find(|i| i.wire == atd_core::registry::WireType::Img)
                .map(|i| i.name.clone());
            (s.type_id.clone(), input)
        })
        .collect();
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let (noise, lfo) = (e.alloc_id(), e.alloc_id());
    e.edit(
        "source",
        [
            Edit::AddNode {
                node: Node::new(noise, "img.noise", "noise"),
            },
            Edit::AddNode {
                node: Node::new(lfo, "sig.lfo", "lfo"),
            },
            Edit::SetParam {
                node: noise,
                param: "z".into(),
                value: Some(Param::bound(json!(0.0), lfo, "lfo")),
            },
        ],
    )
    .unwrap();
    let mut t = 0.0;
    let mut measure = |e: &mut Engine, out: &[NodeId]| {
        for _ in 0..30 {
            e.cook(out, t).unwrap();
            t += 1.0 / 60.0;
        }
        let frames = 40;
        COUNTING.with(|c| c.set(true));
        let before = allocs();
        for _ in 0..frames {
            e.cook(out, t).unwrap();
            t += 1.0 / 60.0;
        }
        let allocs = allocs() - before;
        COUNTING.with(|c| c.set(false));
        allocs as f64 / f64::from(frames)
    };
    let base = measure(&mut e, &[noise]);
    let mut table = Vec::new();
    for (ty, input) in types {
        let id = e.alloc_id();
        let mut edits = vec![Edit::AddNode {
            node: Node::new(id, ty.clone(), "op"),
        }];
        if let Some(input) = input {
            edits.push(Edit::Connect {
                wire: Wire::new(Endpoint::new(noise, "out"), Endpoint::new(id, &input)),
            });
        }
        e.edit("op", edits).unwrap();
        let per_frame = measure(&mut e, &[noise, id]) - base;
        e.edit("remove", [Edit::RemoveNode { id }]).unwrap();
        table.push((per_frame, ty));
    }
    table.sort_by(|a, b| b.0.total_cmp(&a.0));
    let typical = table[table.len() / 2].0.max(1.0);
    // Measured on lavapipe (× typical): fluid 22, reaction-diffusion 15,
    // analyze 6 (readback mapping), 3-pass ISF 5, ripple and time machine 3.
    let allowance = |ty: &str| match ty {
        "img.fluid" => 32.0,
        "img.reaction_diffusion" => 21.0,
        "img.analyze" => 9.0,
        "img.isf_alloc_passes" => 7.0,
        "img.ripple" | "img.time_machine" => 4.0,
        _ => 2.5,
    };
    let mut over = Vec::new();
    for (n, ty) in &table {
        let bound = typical * allowance(ty);
        eprintln!("{n:7.1}  {ty} (bound {bound:.0})");
        if *n > bound {
            over.push(format!("{ty}: {n:.1} > {bound:.0}"));
        }
    }
    assert!(
        over.is_empty(),
        "steady-state allocations per frame over bound (typical {typical:.1}): {over:?}"
    );
}
