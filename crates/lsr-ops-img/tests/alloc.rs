//! Rule 2 for IMG cooks: steady-state frames create no GPU objects (bind
//! groups, views) and allocate little. wgpu itself allocates while
//! recording commands, so the test bounds allocations per node per frame
//! rather than demanding zero.

use std::alloc::{GlobalAlloc, Layout, System};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::Registry;
use lsr_gpu::engine::{Engine, EngineConfig, OpLibrary};
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

/// Tests that cook Share In/Out run one at a time: on WARP (Windows CI),
/// several devices creating D3D11 devices in parallel crash inside WARP
/// (see `tests/share.rs`).
static SHARING: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn img_chain_steady_state_allocations() {
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_sig::library(&mut lib);
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
    let _sharing = SHARING.lock().unwrap_or_else(|e| e.into_inner());
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_sig::library(&mut lib);
    let isf = vec![
        lsr_ops_img::isf::IsfDef::new("Alloc Filter", ISF_FILTER, None).unwrap(),
        lsr_ops_img::isf::IsfDef::new("Alloc Passes", ISF_PASSES, None).unwrap(),
    ];
    lsr_ops_img::isf::register(isf, &mut reg, &mut lib);
    let types: Vec<(String, Option<String>)> = reg
        .iter()
        .filter(|s| s.family == lsr_core::registry::OpFamily::Img)
        .map(|s| {
            let input = s
                .inputs
                .iter()
                .find(|i| i.wire == lsr_core::registry::WireType::Img)
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
        // Up to two passes (pass-through and the 8-bit copy, or keeping
        // and drawing a received frame); 1.1 measured on NVIDIA DX12, up to
        // 2.1 on WARP, depending on how often the worker is busy. See also
        // the Spout/Syphon test below.
        "img.share_out" | "img.share_in" => 3.0,
        // Several dispatches a frame, each recorded by wgpu with about 3.6
        // allocations on NVIDIA DX12 (compute 3.8x typical, neural CA 8x) and
        // more on the CI adapters (compute 3.3x on WARP, 5.1x on Metal).
        "img.compute" => 8.0,
        "img.nca" => 24.0,
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

/// Compute (4 iterations over a ping-ponged state) and Neural CA (8 steps,
/// 18 dispatches) once their pipelines are built (the test above may catch
/// them still compiling): allocations per frame against a one-pass
/// operator's, after a changing source.
#[test]
fn compute_and_nca_steady_state_allocations() {
    let _sharing = SHARING.lock().unwrap_or_else(|e| e.into_inner());
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let [noise, lfo, level, compute, nca] = [(); 5].map(|()| e.alloc_id());
    let code = "#bind input in0\n#bind state s rgba16float from=in0\nfn kernel(id: vec2u) {\n    textureStore(s_out, id, textureLoad(s, vec2i(id), 0) * 0.9);\n}\n";
    e.edit(
        "patch",
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
            Edit::AddNode {
                node: Node::new(level, "img.level", "level"),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(noise, "out"), Endpoint::new(level, "in")),
            },
            Edit::AddNode {
                node: Node::new(compute, "img.compute", "compute"),
            },
            Edit::SetParam {
                node: compute,
                param: "code".into(),
                value: Some(Param::constant(json!(code))),
            },
            Edit::SetParam {
                node: compute,
                param: "iterations".into(),
                value: Some(Param::constant(json!(4))),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(noise, "out"), Endpoint::new(compute, "in0")),
            },
            Edit::AddNode {
                node: Node::new(nca, "img.nca", "nca"),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(noise, "out"), Endpoint::new(nca, "erase")),
            },
        ],
    )
    .unwrap();
    let mut t = 0.0;
    // Until both have compiled.
    let start = std::time::Instant::now();
    loop {
        e.cook(&[level, compute, nca], t).unwrap();
        t += 1.0 / 60.0;
        if !e.pending() {
            break;
        }
        assert!(
            start.elapsed().as_secs() < 60,
            "compiled (errors {:?}, {:?})",
            e.error(compute),
            e.error(nca)
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(e.error(compute), None);
    assert_eq!(e.error(nca), None);
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
    let one_pass = (measure(&mut e, &[noise, level]) - base).max(1.0);
    let compute_n = measure(&mut e, &[noise, compute]) - base;
    let nca_n = measure(&mut e, &[noise, nca]) - base;
    eprintln!(
        "per frame beyond the source: one pass {one_pass:.1}, compute (5 dispatches + copy) {compute_n:.1}, neural CA (18 dispatches + copy) {nca_n:.1}"
    );
    // wgpu allocates while recording each pass and each dispatch (about 3.6
    // per dispatch on NVIDIA DX12, where one pass is 15, compute 57 and the
    // neural CA 119); creating bind groups or views per frame would add far
    // more. Bound: two passes plus 1.2 passes per dispatch: Metal on the CI
    // runner measured compute at 88 where half a pass per dispatch allowed 78.
    let bound = |dispatches: f64| one_pass * (2.0 + 1.2 * dispatches);
    assert!(
        compute_n <= bound(5.0),
        "compute: {compute_n:.1} > {:.1}",
        bound(5.0)
    );
    assert!(
        nca_n <= bound(18.0),
        "neural CA: {nca_n:.1} > {:.1}",
        bound(18.0)
    );
}

/// Share Out publishing a changing image to a Share In in the same engine
/// over Spout (Windows, DX12) or Syphon (macOS, Metal): what the cook
/// thread allocates per frame. The workers' Spout or Syphon calls run on
/// their own threads (not counted here; `lsr-interop` keeps Spout's
/// allocation-free after warm-up, while Syphon's Objective-C calls make
/// command buffers and autoreleased objects on the worker).
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn share_out_and_in_steady_state_allocations() {
    let _sharing = SHARING.lock().unwrap_or_else(|e| e.into_inner());
    let instance = lsr_gpu::primary_instance();
    let backend = if cfg!(windows) {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::METAL
    };
    let adapters = pollster::block_on(instance.enumerate_adapters(backend));
    let Some(adapter) = adapters
        .into_iter()
        .min_by_key(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no {backend:?} adapter"
        );
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut reg = Registry::default();
    lsr_ops_img::register(&mut reg);
    lsr_ops_sig::register(&mut reg);
    let mut lib = OpLibrary::default();
    lsr_ops_img::library(&mut lib);
    lsr_ops_sig::library(&mut lib);
    let config = EngineConfig {
        width: 64,
        height: 32,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    let name = format!("lsr-test-{}-alloc", std::process::id());
    let [noise, lfo, out, inp] = [(); 4].map(|()| e.alloc_id());
    e.edit(
        "patch",
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
            Edit::AddNode {
                node: Node::new(out, "img.share_out", "out"),
            },
            Edit::SetParam {
                node: out,
                param: "name".into(),
                value: Some(Param::constant(json!(name))),
            },
            Edit::Connect {
                wire: Wire::new(Endpoint::new(noise, "out"), Endpoint::new(out, "in")),
            },
            Edit::AddNode {
                node: Node::new(inp, "img.share_in", "in"),
            },
            Edit::SetParam {
                node: inp,
                param: "name".into(),
                value: Some(Param::constant(json!(name))),
            },
        ],
    )
    .unwrap();
    let mut t = 0.0;
    let mut frame = |e: &mut Engine, nodes: &[NodeId]| {
        e.cook(nodes, t).unwrap();
        t += 1.0 / 60.0;
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    // Warm up: the sender exists and Share In is receiving.
    let start = std::time::Instant::now();
    while !e.status(inp).is_some_and(|s| s.starts_with("Receiving")) {
        assert!(
            start.elapsed().as_secs() < 30,
            "no loopback: {:?}",
            e.status(inp)
        );
        frame(&mut e, &[out, inp]);
    }
    for _ in 0..30 {
        frame(&mut e, &[out, inp]);
    }
    let frames = 60;
    let mut count = |e: &mut Engine, nodes: &[NodeId]| {
        COUNTING.with(|c| c.set(true));
        let before = allocs();
        for _ in 0..frames {
            frame(e, nodes);
        }
        let n = allocs() - before;
        COUNTING.with(|c| c.set(false));
        n as f64 / f64::from(frames)
    };
    let both = count(&mut e, &[out, inp]);
    let base = count(&mut e, &[noise]);
    assert_eq!(e.error(out), None);
    assert_eq!(e.error(inp), None);
    let per_node = (both - base) / 2.0;
    eprintln!(
        "Share Out + Share In: {both:.1} allocations per frame, {base:.1} for the source alone, {per_node:.1} per node-cook"
    );
    assert!(
        per_node <= 2.0 * BOUND,
        "{per_node:.1} allocations per node-cook (bound {})",
        2.0 * BOUND
    );
    // The workers stop their Syphon or Spout objects once the nodes are
    // gone, and the last one tears the device down; exiting before they
    // finish trapped on the macOS runner. Usually under 0.1 s, but up to
    // 16 s measured with four test binaries sharing the GPU.
    drop(e);
    assert!(
        lsr_ops_img::share_wait_for_workers(std::time::Duration::from_secs(60)),
        "the Share workers did not finish"
    );
}
